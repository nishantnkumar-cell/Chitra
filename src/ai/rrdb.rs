//! Real-ESRGAN's large model (RRDBNet, from ESRGAN): a trunk of
//! residual-in-residual dense blocks at the input size, then two
//! nearest-neighbour 2x steps with convolutions. The 2x models first fold
//! each 2x2 block of pixels into 12 channels, so the trunk runs at half size.

use rayon::prelude::*;

use super::conv::{Act, Conv, Geo, conv3x3, pad, up2};
use super::pth::Weights;

const FEAT: usize = 64;
const GROW: usize = 32;
/// Channels per pixel of a dense block's working buffer: the block input
/// and the four 32-channel layers that each see everything before them.
const WIDE: usize = FEAT + 4 * GROW;
const LEAKY: Act<'static> = Act::Leaky(0.2);

#[cfg(feature = "gpu")]
pub(super) struct Parts<'a> {
    pub first: &'a Conv,
    pub blocks: &'a [[[Conv; 5]; 3]],
    pub body: &'a Conv,
    pub up1: &'a Conv,
    pub up2: &'a Conv,
    pub hr: &'a Conv,
    pub last: &'a Conv,
    pub fold: usize,
}

pub(super) struct Net {
    first: Conv,
    /// `blocks[b][r][l]`: layer `l` of dense block `r` of residual block `b`.
    blocks: Vec<[[Conv; 5]; 3]>,
    body: Conv,
    up1: Conv,
    up2: Conv,
    hr: Conv,
    last: Conv,
    /// 1 for the 4x models, 2 when the input is folded first (2x models).
    fold: usize,
}

impl Net {
    pub fn load(w: &Weights) -> Result<Net, String> {
        let first = Conv::load(w, "conv_first")?;
        let fold = match first.cin {
            3 => 1,
            12 => 2,
            n => return Err(format!("unexpected model input of {n} channels")),
        };
        let mut blocks = Vec::new();
        while w.has(&format!("body.{}.rdb1.conv1.weight", blocks.len())) {
            let b = blocks.len();
            let dense = |r: usize| -> Result<[Conv; 5], String> {
                let l = |i: usize| Conv::load(w, &format!("body.{b}.rdb{r}.conv{i}"));
                Ok([l(1)?, l(2)?, l(3)?, l(4)?, l(5)?])
            };
            blocks.push([dense(1)?, dense(2)?, dense(3)?]);
        }
        if blocks.is_empty() {
            return Err("model has no blocks".into());
        }
        let net = Net {
            first,
            blocks,
            body: Conv::load(w, "conv_body")?,
            up1: Conv::load(w, "conv_up1")?,
            up2: Conv::load(w, "conv_up2")?,
            hr: Conv::load(w, "conv_hr")?,
            last: Conv::load(w, "conv_last")?,
            fold,
        };
        let shapes_ok = net.first.cout == FEAT
            && net.last.cout == 3
            && net.blocks.iter().flatten().all(|d| {
                d.iter().enumerate().all(|(i, c)| {
                    c.cin == FEAT + i * GROW && c.cout == if i < 4 { GROW } else { FEAT }
                })
            });
        if !shapes_ok {
            return Err("unexpected model layout".into());
        }
        Ok(net)
    }

    /// The layers, for uploading to the graphics card.
    #[cfg(feature = "gpu")]
    pub fn parts(&self) -> Parts<'_> {
        Parts {
            first: &self.first,
            blocks: &self.blocks,
            body: &self.body,
            up1: &self.up1,
            up2: &self.up2,
            hr: &self.hr,
            last: &self.last,
            fold: self.fold,
        }
    }

    /// How much the model enlarges.
    pub fn scale(&self) -> usize {
        4 / self.fold
    }

    /// Run on one `w x h` RGB tile (0..1, row-major); `w` and `h` must be
    /// multiples of the fold. Gives the enlarged tile.
    pub fn run(&self, rgb: &[f32], w: usize, h: usize) -> Vec<f32> {
        let f = self.fold;
        let g = Geo { w: w / f, h: h / f };
        let input = if f == 1 {
            rgb.to_vec()
        } else {
            unfold(rgb, w, h, f)
        };
        let feat = conv3x3(
            &pad(&input, g, 3 * f * f),
            3 * f * f,
            g,
            &self.first,
            Act::None,
        );

        // The trunk works in one wide buffer: channels 0..64 hold the running
        // features, the dense layers write their outputs after them.
        let n = g.pixels();
        let mut wide = vec![0f32; n * WIDE];
        put(&mut wide, &feat, FEAT, 0);
        for block in &self.blocks {
            let start = take(&wide, FEAT, 0);
            for dense in block {
                for (i, layer) in dense[..4].iter().enumerate() {
                    let out = conv3x3(&wide, WIDE, g, layer, LEAKY);
                    put(&mut wide, &out, GROW, FEAT + i * GROW);
                }
                let out = conv3x3(&wide, WIDE, g, &dense[4], Act::None);
                wide.par_chunks_mut(WIDE)
                    .zip(out.par_chunks(FEAT))
                    .for_each(|(px, o)| {
                        for (v, d) in px[..FEAT].iter_mut().zip(o) {
                            *v += 0.2 * d;
                        }
                    });
            }
            wide.par_chunks_mut(WIDE)
                .zip(start.par_chunks(FEAT))
                .for_each(|(px, s)| {
                    for (v, s) in px[..FEAT].iter_mut().zip(s) {
                        *v = 0.2 * *v + s;
                    }
                });
        }
        let mut trunk = conv3x3(&wide, WIDE, g, &self.body, Act::None);
        drop(wide);
        trunk.par_iter_mut().zip(&feat).for_each(|(t, f)| *t += f);

        let g2 = Geo {
            w: g.w * 2,
            h: g.h * 2,
        };
        let x = conv3x3(&up2(&trunk, g, FEAT), FEAT, g2, &self.up1, LEAKY);
        let g4 = Geo {
            w: g.w * 4,
            h: g.h * 4,
        };
        let x = conv3x3(&up2(&x, g2, FEAT), FEAT, g4, &self.up2, LEAKY);
        let x = conv3x3(&x, FEAT, g4, &self.hr, LEAKY);
        let x = conv3x3(&x, FEAT, g4, &self.last, Act::None);

        let (ow, oh, wp) = (g4.w, g4.h, g4.wp());
        let mut out = vec![0f32; ow * oh * 3];
        out.par_chunks_mut(ow * 3)
            .enumerate()
            .for_each(|(y, line)| {
                let row = &x[((y + 1) * wp + 1) * 3..][..ow * 3];
                for (o, v) in line.iter_mut().zip(row) {
                    *o = v.clamp(0.0, 1.0);
                }
            });
        out
    }
}

/// Copy a `c`-channel padded buffer into channels `at..at + c` of `wide`.
fn put(wide: &mut [f32], src: &[f32], c: usize, at: usize) {
    wide.par_chunks_mut(WIDE)
        .zip(src.par_chunks(c))
        .for_each(|(px, s)| px[at..at + c].copy_from_slice(s));
}

/// Channels `at..at + c` of `wide` as their own buffer.
fn take(wide: &[f32], c: usize, at: usize) -> Vec<f32> {
    let mut out = vec![0f32; wide.len() / WIDE * c];
    out.par_chunks_mut(c)
        .zip(wide.par_chunks(WIDE))
        .for_each(|(o, px)| o.copy_from_slice(&px[at..at + c]));
    out
}

/// Fold each `f x f` block into `3 f f` channels, in PyTorch's
/// `pixel_unshuffle` order (colour, row in block, column in block).
pub(super) fn unfold(rgb: &[f32], w: usize, h: usize, f: usize) -> Vec<f32> {
    let (sw, sh, c) = (w / f, h / f, 3 * f * f);
    let mut out = vec![0f32; sw * sh * c];
    for y in 0..sh {
        for x in 0..sw {
            let o = &mut out[(y * sw + x) * c..][..c];
            for ch in 0..3 {
                for i in 0..f {
                    for j in 0..f {
                        o[ch * f * f + i * f + j] = rgb[((y * f + i) * w + x * f + j) * 3 + ch];
                    }
                }
            }
        }
    }
    out
}
