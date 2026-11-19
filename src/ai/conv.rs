//! 3x3 convolutions on the CPU as matrix products.
//!
//! Feature maps are stored as padded HWC buffers: `(w + 2) x (h + 2)` pixels
//! with a zero border, `stride` floats per pixel. A convolution is nine
//! matrix products, one per kernel position, of the shifted input with that
//! position's `cin x cout` weights (`ndarray`/`matrixmultiply`, which pick
//! AVX/FMA or NEON at run time). Reading only the first `cin` channels of a
//! wider buffer lets densely connected layers skip concatenation.

use ndarray::linalg::general_mat_mul;
use ndarray::{ArrayView2, ArrayViewMut2, ShapeBuilder};
use rayon::prelude::*;

use super::pth::Weights;

/// One 3x3 convolution with padding 1.
#[derive(Clone)]
pub(super) struct Conv {
    pub cin: usize,
    pub cout: usize,
    /// `w[k]` is a row-major `cin x cout` matrix for kernel position `k`.
    pub w: Vec<f32>,
    pub bias: Vec<f32>,
}

impl Conv {
    /// Read `<name>.weight` (`[cout][cin][3][3]`) and `<name>.bias`.
    pub fn load(wts: &Weights, name: &str) -> Result<Conv, String> {
        let shape = wts.shape(&format!("{name}.weight"))?.to_vec();
        let [cout, cin, 3, 3] = shape[..] else {
            return Err(format!("{name} is not a 3x3 convolution"));
        };
        let raw = wts.get(&format!("{name}.weight"), &shape)?;
        let mut w = vec![0f32; 9 * cin * cout];
        for co in 0..cout {
            for ci in 0..cin {
                for k in 0..9 {
                    w[k * cin * cout + ci * cout + co] = raw[(co * cin + ci) * 9 + k];
                }
            }
        }
        let bias = wts.get(&format!("{name}.bias"), &[cout])?;
        Ok(Conv { cin, cout, w, bias })
    }

    pub fn mix(&self, other: &Conv, t: f32) -> Conv {
        let mix = |x: &[f32], y: &[f32]| -> Vec<f32> {
            x.iter()
                .zip(y)
                .map(|(p, q)| t * p + (1.0 - t) * q)
                .collect()
        };
        Conv {
            cin: self.cin,
            cout: self.cout,
            w: mix(&self.w, &other.w),
            bias: mix(&self.bias, &other.bias),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum Act<'a> {
    None,
    /// PReLU with one slope per output channel.
    PRelu(&'a [f32]),
    Leaky(f32),
}

/// Size of a feature map without its border.
#[derive(Clone, Copy)]
pub(super) struct Geo {
    pub w: usize,
    pub h: usize,
}

impl Geo {
    pub fn wp(self) -> usize {
        self.w + 2
    }
    pub fn pixels(self) -> usize {
        (self.w + 2) * (self.h + 2)
    }
}

/// Convolve the first `conv.cin` channels of `src` (`stride` per pixel) into
/// a new padded buffer of `conv.cout` channels, with bias and activation.
pub(super) fn conv3x3(src: &[f32], stride: usize, g: Geo, conv: &Conv, act: Act) -> Vec<f32> {
    let (cin, cout, wp, hp) = (conv.cin, conv.cout, g.wp(), g.h + 2);
    let mut out = vec![0f32; hp * wp * cout];
    // Output row o = y * wp + x for the pixel (x, y); its input for kernel
    // position (ky, kx) is padded pixel o + ky * wp + kx. The two border
    // pixels between image rows are computed too and cleared below.
    let m = g.h * wp - 2;
    {
        let base = (wp + 1) * cout;
        let mut c = ArrayViewMut2::from_shape(
            (m, cout).strides((cout, 1)),
            &mut out[base..base + m * cout],
        )
        .expect("output view");
        for k in 0..9 {
            let off = ((k / 3) * wp + k % 3) * stride;
            let a = ArrayView2::from_shape(
                (m, cin).strides((stride, 1)),
                &src[off..off + (m - 1) * stride + cin],
            )
            .expect("input view");
            let b = ArrayView2::from_shape((cin, cout), &conv.w[k * cin * cout..][..cin * cout])
                .expect("weight view");
            general_mat_mul(1.0, &a, &b, 1.0, &mut c);
        }
    }
    out.par_chunks_mut(wp * cout)
        .enumerate()
        .for_each(|(row, line)| {
            if row == 0 || row == hp - 1 {
                line.iter_mut().for_each(|v| *v = 0.0);
                return;
            }
            line[..cout].iter_mut().for_each(|v| *v = 0.0);
            line[(wp - 1) * cout..].iter_mut().for_each(|v| *v = 0.0);
            for px in line[cout..(wp - 1) * cout].chunks_exact_mut(cout) {
                for (i, v) in px.iter_mut().enumerate() {
                    let x = *v + conv.bias[i];
                    *v = match act {
                        Act::None => x,
                        Act::PRelu(s) => {
                            if x >= 0.0 {
                                x
                            } else {
                                x * s[i]
                            }
                        }
                        Act::Leaky(s) => {
                            if x >= 0.0 {
                                x
                            } else {
                                x * s
                            }
                        }
                    };
                }
            }
        });
    out
}

/// A padded buffer holding an unpadded `w x h` image of `c` channels.
pub(super) fn pad(src: &[f32], g: Geo, c: usize) -> Vec<f32> {
    let wp = g.wp();
    let mut out = vec![0f32; g.pixels() * c];
    for y in 0..g.h {
        let dst = ((y + 1) * wp + 1) * c;
        out[dst..dst + g.w * c].copy_from_slice(&src[y * g.w * c..(y + 1) * g.w * c]);
    }
    out
}

/// Nearest-neighbour 2x of a padded buffer of `c` channels.
pub(super) fn up2(src: &[f32], g: Geo, c: usize) -> Vec<f32> {
    let big = Geo {
        w: g.w * 2,
        h: g.h * 2,
    };
    let (wp, bwp) = (g.wp(), big.wp());
    let mut out = vec![0f32; big.pixels() * c];
    out.par_chunks_mut(bwp * c)
        .enumerate()
        .for_each(|(by, line)| {
            if by == 0 || by > big.h {
                return;
            }
            let y = (by - 1) / 2 + 1;
            for bx in 1..=big.w {
                let x = (bx - 1) / 2 + 1;
                line[bx * c..(bx + 1) * c].copy_from_slice(&src[(y * wp + x) * c..][..c]);
            }
        });
    out
}
