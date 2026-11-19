//! Real-ESRGAN's compact model (SRVGGNetCompact): 34 3x3 convolutions of 64
//! channels with PReLU, a 4x pixel shuffle, and the input enlarged
//! nearest-neighbour added back.

use rayon::prelude::*;

use super::conv::{Act, Conv, Geo, conv3x3, pad};
use super::pth::Weights;

pub(super) const CONVS: usize = 34;

#[derive(Clone)]
pub(super) struct Net {
    pub convs: Vec<Conv>,
    /// PReLU slopes after every convolution but the last.
    pub prelu: Vec<Vec<f32>>,
}

impl Net {
    pub fn load(w: &Weights) -> Result<Net, String> {
        let mut convs = Vec::with_capacity(CONVS);
        let mut prelu = Vec::with_capacity(CONVS - 1);
        for j in 0..CONVS {
            let c = Conv::load(w, &format!("body.{}", 2 * j))?;
            if j < CONVS - 1 {
                prelu.push(w.get(&format!("body.{}.weight", 2 * j + 1), &[c.cout])?);
            }
            convs.push(c);
        }
        if convs[0].cin != 3 || convs[CONVS - 1].cout != 48 {
            return Err("unexpected model layout".into());
        }
        Ok(Net { convs, prelu })
    }

    /// `t * a + (1 - t) * b`, as Real-ESRGAN blends its two general models.
    pub fn blend(a: &Net, b: &Net, t: f32) -> Net {
        Net {
            convs: a
                .convs
                .iter()
                .zip(&b.convs)
                .map(|(p, q)| p.mix(q, t))
                .collect(),
            prelu: a
                .prelu
                .iter()
                .zip(&b.prelu)
                .map(|(p, q)| {
                    p.iter()
                        .zip(q)
                        .map(|(x, y)| t * x + (1.0 - t) * y)
                        .collect()
                })
                .collect(),
        }
    }

    /// Run on one `w x h` RGB tile (0..1, row-major), giving the 4x result.
    pub fn run(&self, rgb: &[f32], w: usize, h: usize) -> Vec<f32> {
        let g = Geo { w, h };
        let mut cur = pad(rgb, g, 3);
        let mut stride = 3;
        for (j, conv) in self.convs.iter().enumerate() {
            let act = match self.prelu.get(j) {
                Some(s) => Act::PRelu(s),
                None => Act::None,
            };
            cur = conv3x3(&cur, stride, g, conv, act);
            stride = conv.cout;
        }
        shuffle(&cur, rgb, w, h)
    }
}

/// The last layer's 48 channels (padded) to the 4x image: pixel shuffle
/// (channel c * 16 + i * 4 + j goes to sub-pixel (j, i) of colour c) plus the
/// nearest-neighbour enlargement of the input.
pub(super) fn shuffle(cur: &[f32], rgb: &[f32], w: usize, h: usize) -> Vec<f32> {
    let g = Geo { w, h };
    let (wp, ow, oh) = (g.wp(), w * 4, h * 4);
    let mut out = vec![0f32; ow * oh * 3];
    out.par_chunks_mut(ow * 3)
        .enumerate()
        .for_each(|(oy, line)| {
            let (y, i) = (oy / 4, oy % 4);
            for ox in 0..ow {
                let (x, j) = (ox / 4, ox % 4);
                let feat = &cur[((y + 1) * wp + x + 1) * 48..][..48];
                for c in 0..3 {
                    let v = feat[c * 16 + i * 4 + j] + rgb[(y * w + x) * 3 + c];
                    line[ox * 3 + c] = v.clamp(0.0, 1.0);
                }
            }
        });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A net whose last layer is all zeros gives plain nearest-neighbour 4x.
    #[test]
    fn zero_net_is_nearest_neighbour() {
        let conv = |cin: usize, cout: usize, v: f32| Conv {
            cin,
            cout,
            w: vec![v; 9 * cin * cout],
            bias: vec![0.0; cout],
        };
        let mut convs: Vec<Conv> = (0..CONVS - 1)
            .map(|j| conv(if j == 0 { 3 } else { 64 }, 64, 0.01))
            .collect();
        convs.push(conv(64, 48, 0.0));
        let net = Net {
            convs,
            prelu: vec![vec![0.1; 64]; CONVS - 1],
        };
        let rgb: Vec<f32> = (0..5 * 3 * 3).map(|i| (i % 7) as f32 / 7.0).collect();
        let out = net.run(&rgb, 5, 3);
        assert_eq!(out.len(), 20 * 12 * 3);
        for oy in 0..12 {
            for ox in 0..20 {
                for c in 0..3 {
                    let want = rgb[((oy / 4) * 5 + ox / 4) * 3 + c];
                    assert!((out[(oy * 20 + ox) * 3 + c] - want).abs() < 1e-6);
                }
            }
        }
    }
}
