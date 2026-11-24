//! Running the AI models on the graphics card with `wgpu` (Vulkan, DirectX 12
//! or Metal), when one is available.
//!
//! The same networks as on the processor, written as three small compute
//! shaders: a 3x3 convolution (tiled through workgroup memory), a scaled sum
//! for the residual connections, and a nearest-neighbour 2x. All feature maps
//! of a tile live in one storage buffer (the arena) as padded HWC regions
//! like the CPU ones, so dense layers read the first channels of a wide map
//! and write the next ones without copies. Anything that goes wrong (no
//! adapter, too little memory, a lost device) falls back to the processor.

use std::sync::{Arc, Mutex};

use wgpu::util::DeviceExt;

use super::compact;
use super::conv::{Act, Conv};
use super::rrdb;
use super::{AdapterEntry, Compute, Ready, crop, place, tiles};

const SHADER: &str = include_str!("gpu.wgsl");
/// Bytes per parameter block; uniform offsets must be 256-aligned.
const PARAM_STRIDE: u64 = 256;
/// Largest arena to ask for, whatever the card allows.
const ARENA_CAP: u64 = 768 << 20;
/// Dispatches per submission, so slow cards never run one huge batch.
const PER_SUBMIT: usize = 48;
/// Arithmetic per submission (multiply-adds x 2). A small integrated GPU
/// does a few hundred GFLOP/s here, and Linux resets a GPU whose job runs
/// for more than a few seconds, losing the device.
const FLOPS_PER_SUBMIT: f64 = 40e9;
/// Largest tile on an integrated GPU, so one convolution stays short.
const INTEGRATED_TILE: usize = 512;

/// Set when the driver reports the device lost (for example after a GPU
/// reset); nothing is sent to the card after that.
static LOST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Longest wait for one tile before falling back to the processor.
const TILE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    name: String,
    conv: wgpu::ComputePipeline,
    mix: wgpu::ComputePipeline,
    up: wgpu::ComputePipeline,
    conv_layout: wgpu::BindGroupLayout,
    simple_layout: wgpu::BindGroupLayout,
    dummy: wgpu::Buffer,
    max_binding: u64,
    /// Largest tile side to use (smaller on integrated GPUs).
    max_tile: usize,
    /// Set after a failure; the processor is used from then on.
    broken: Mutex<bool>,
}

/// The card in use for the current choice (`None` inside: none usable).
static GPU: Mutex<Option<(Compute, Option<Arc<Gpu>>)>> = Mutex::new(None);

/// Why the graphics card is not (or no longer) used, for the AI window.
static WHY: Mutex<String> = Mutex::new(String::new());

fn note(msg: impl Into<String>) {
    if let Ok(mut w) = WHY.lock() {
        *w = msg.into();
    }
}

pub(super) fn why_not() -> String {
    WHY.lock().map(|w| w.clone()).unwrap_or_default()
}

/// Weights uploaded for one network, keyed by the network's address.
static NETS: Mutex<Option<(usize, Arc<GpuNet>)>> = Mutex::new(None);

pub(super) fn forget() {
    if let Ok(mut n) = NETS.lock() {
        *n = None;
    }
}

/// Look at the graphics cards again and start over with the next run.
pub(super) fn reset() {
    forget();
    if let Ok(mut g) = GPU.lock() {
        *g = None;
    }
    LOST.store(false, std::sync::atomic::Ordering::SeqCst);
    note("");
}

fn gpu() -> Option<Arc<Gpu>> {
    let want = super::compute();
    if want == Compute::Processor {
        return None;
    }
    let mut slot = GPU.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((c, g)) = slot.as_ref()
        && *c == want
    {
        return g.clone();
    }
    // A different choice: the old card's weights are of no use.
    forget();
    LOST.store(false, std::sync::atomic::Ordering::SeqCst);
    note("");
    let g = pollster::block_on(init(&want)).map(Arc::new);
    *slot = Some((want, g.clone()));
    g
}

fn instance() -> wgpu::Instance {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
    desc.backends = wgpu::Backends::VULKAN | wgpu::Backends::DX12 | wgpu::Backends::METAL;
    wgpu::Instance::new(desc)
}

fn kind(t: wgpu::DeviceType) -> &'static str {
    match t {
        wgpu::DeviceType::DiscreteGpu => "discrete",
        wgpu::DeviceType::IntegratedGpu => "integrated",
        wgpu::DeviceType::VirtualGpu => "virtual",
        wgpu::DeviceType::Cpu => "software",
        wgpu::DeviceType::Other => "other",
    }
}

/// Lower is better: a discrete card, then an integrated one. Software
/// renderers are slower than the processor path and are never picked.
fn rank(t: wgpu::DeviceType) -> Option<u8> {
    match t {
        wgpu::DeviceType::DiscreteGpu => Some(0),
        wgpu::DeviceType::IntegratedGpu => Some(1),
        wgpu::DeviceType::VirtualGpu => Some(2),
        wgpu::DeviceType::Other => Some(3),
        wgpu::DeviceType::Cpu => None,
    }
}

/// Every adapter the system offers, with the one Automatic would use.
pub(super) fn adapters() -> (Vec<AdapterEntry>, Option<String>) {
    let list =
        pollster::block_on(instance().enumerate_adapters(
            wgpu::Backends::VULKAN | wgpu::Backends::DX12 | wgpu::Backends::METAL,
        ));
    let mut out: Vec<AdapterEntry> = Vec::new();
    let mut best: Option<(u8, String)> = None;
    for a in &list {
        let i = a.get_info();
        if out
            .iter()
            .any(|e| e.name == i.name && e.backend == format!("{:?}", i.backend))
        {
            continue;
        }
        if let Some(r) = rank(i.device_type)
            && best.as_ref().is_none_or(|(b, _)| r < *b)
        {
            best = Some((r, i.name.clone()));
        }
        out.push(AdapterEntry {
            name: i.name,
            kind: kind(i.device_type).to_string(),
            backend: format!("{:?}", i.backend),
        });
    }
    (out, best.map(|(_, n)| n))
}

async fn init(want: &Compute) -> Option<Gpu> {
    let instance = instance();
    let all = instance
        .enumerate_adapters(wgpu::Backends::VULKAN | wgpu::Backends::DX12 | wgpu::Backends::METAL)
        .await;
    let allow_cpu = std::env::var_os("CHITRA_GPU_ALLOW_CPU").is_some();
    // The chosen card by name (Vulkan before DirectX 12 when it shows up in
    // both), or the best one by type.
    let named = match want {
        Compute::Adapter(n) => all
            .iter()
            .filter(|a| a.get_info().name == *n)
            .min_by_key(|a| match a.get_info().backend {
                wgpu::Backend::Vulkan => 0,
                wgpu::Backend::Dx12 => 1,
                _ => 2,
            })
            .cloned(),
        _ => None,
    };
    if named.is_none() && matches!(want, Compute::Adapter(_)) {
        note("the chosen graphics card was not found; using Automatic");
    }
    let adapter = match named {
        Some(a) => a,
        None => all
            .iter()
            .filter(|a| {
                let t = a.get_info().device_type;
                rank(t).is_some() || allow_cpu
            })
            .min_by_key(|a| rank(a.get_info().device_type).unwrap_or(9))
            .cloned()
            .or_else(|| {
                note("no graphics card with Vulkan, DirectX 12 or Metal support was found");
                None
            })?,
    };
    let info = adapter.get_info();
    // Software renderers are slower than the CPU path.
    if info.device_type == wgpu::DeviceType::Cpu && !allow_cpu {
        note("only a software renderer was found");
        return None;
    }
    let limits = adapter.limits();
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("chitra-ai"),
            required_limits: limits.clone(),
            ..Default::default()
        })
        .await
        .map_err(|e| note(format!("the graphics card could not be opened: {e}")))
        .ok()?;
    device.on_uncaptured_error(Arc::new(|e| {
        eprintln!("graphics card error: {e}");
    }));
    device.set_device_lost_callback(|reason, msg| {
        eprintln!("graphics card lost ({reason:?}): {msg}; using the processor");
        note(format!(
            "the graphics card was lost ({reason:?}); using the processor"
        ));
        LOST.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("chitra-ai"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });
    let storage = |binding, read_only| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    };
    let uniform = |binding| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    };
    let conv_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("conv"),
        entries: &[
            storage(0, false),
            storage(1, true),
            storage(2, true),
            storage(3, true),
            uniform(4),
        ],
    });
    let simple_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("simple"),
        entries: &[storage(0, false), uniform(4)],
    });
    let pipeline = |layout: &wgpu::BindGroupLayout, entry: &str| {
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(entry),
            bind_group_layouts: &[Some(layout)],
            immediate_size: 0,
        });
        device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(entry),
            layout: Some(&pl),
            module: &module,
            entry_point: Some(entry),
            compilation_options: Default::default(),
            cache: None,
        })
    };
    let conv = pipeline(&conv_layout, "conv");
    let mix = pipeline(&simple_layout, "mix");
    let up = pipeline(&simple_layout, "up");
    let dummy = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("dummy"),
        contents: &[0u8; 16],
        usage: wgpu::BufferUsages::STORAGE,
    });
    Some(Gpu {
        max_tile: if info.device_type == wgpu::DeviceType::IntegratedGpu {
            INTEGRATED_TILE
        } else {
            1024
        },
        name: info.name,
        max_binding: limits
            .max_storage_buffer_binding_size
            .min(limits.max_buffer_size),
        device,
        queue,
        conv,
        mix,
        up,
        conv_layout,
        simple_layout,
        dummy,
        broken: Mutex::new(false),
    })
}

// ----- uploaded networks ----------------------------------------------------------

struct GpuConv {
    cin: usize,
    cout: usize,
    w: wgpu::Buffer,
    bias: wgpu::Buffer,
    slopes: Option<wgpu::Buffer>,
}

#[allow(clippy::large_enum_variant)] // one per process, kept in an Arc
enum GpuNet {
    Compact(Vec<GpuConv>),
    Rrdb {
        first: GpuConv,
        blocks: Vec<[[GpuConv; 5]; 3]>,
        body: GpuConv,
        up1: GpuConv,
        up2: GpuConv,
        hr: GpuConv,
        last: GpuConv,
        fold: usize,
    },
}

fn bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn upload(g: &Gpu, c: &Conv, slopes: Option<&[f32]>) -> GpuConv {
    // Output channels padded to a multiple of 4: the shader reads vec4s.
    let c4 = c.cout.next_multiple_of(4);
    let mut w = vec![0f32; 9 * c.cin * c4];
    for k in 0..9 {
        for ci in 0..c.cin {
            let src = &c.w[(k * c.cin + ci) * c.cout..][..c.cout];
            w[(k * c.cin + ci) * c4..][..c.cout].copy_from_slice(src);
        }
    }
    let buf = |label, data: &[f32]| {
        g.device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: &bytes(data),
                usage: wgpu::BufferUsages::STORAGE,
            })
    };
    GpuConv {
        cin: c.cin,
        cout: c.cout,
        w: buf("weights", &w),
        bias: buf("bias", &c.bias),
        slopes: slopes.map(|s| buf("slopes", s)),
    }
}

fn upload_net(g: &Gpu, net: &Ready) -> GpuNet {
    match net {
        Ready::Compact(n) => GpuNet::Compact(
            n.convs
                .iter()
                .enumerate()
                .map(|(j, c)| upload(g, c, n.prelu.get(j).map(|s| s.as_slice())))
                .collect(),
        ),
        Ready::Rrdb(n) => {
            let p = n.parts();
            let up = |c: &Conv| upload(g, c, None);
            GpuNet::Rrdb {
                first: up(p.first),
                blocks: p
                    .blocks
                    .iter()
                    .map(|b| b.each_ref().map(|d| d.each_ref().map(up)))
                    .collect(),
                body: up(p.body),
                up1: up(p.up1),
                up2: up(p.up2),
                hr: up(p.hr),
                last: up(p.last),
                fold: p.fold,
            }
        }
    }
}

fn net_on_gpu(g: &Gpu, net: &Ready) -> Arc<GpuNet> {
    let key = match net {
        Ready::Compact(n) => Arc::as_ptr(n) as usize,
        Ready::Rrdb(n) => Arc::as_ptr(n) as usize,
    };
    let mut cache = NETS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((k, n)) = cache.as_ref()
        && *k == key
    {
        return n.clone();
    }
    *cache = None;
    let n = Arc::new(upload_net(g, net));
    *cache = Some((key, n.clone()));
    n
}

// ----- the arena ------------------------------------------------------------------

/// Padded pixels of a `w x h` map.
fn px(w: usize, h: usize) -> usize {
    (w + 2) * (h + 2)
}

/// Where each map of one tile starts in the arena, in floats.
#[derive(Default)]
struct Layout {
    /// Input (RGB, or 12 folded channels), then the result to read back.
    input: usize,
    out: usize,
    out_len: usize,
    total: usize,
    // Compact: two ping-pong maps. Rrdb: the trunk and enlargement maps.
    a: usize,
    b: usize,
    feat: usize,
    wide: usize,
    start: usize,
    tmp: usize,
    u1a: usize,
    u1b: usize,
    u2a: usize,
    u2b: usize,
}

impl Layout {
    fn new(net: &GpuNet, pw: usize, ph: usize) -> Layout {
        let mut l = Layout::default();
        let mut at = 0;
        let mut take = |n: usize| {
            let s = at;
            at += n;
            s
        };
        match net {
            GpuNet::Compact(_) => {
                let n = px(pw, ph);
                l.input = take(n * 3);
                l.a = take(n * 64);
                l.b = take(n * 64);
                l.out_len = n * 48;
                l.out = take(l.out_len);
            }
            GpuNet::Rrdb { fold, .. } => {
                let (w, h) = (pw / fold, ph / fold);
                let (n1, n2, n4) = (px(w, h), px(2 * w, 2 * h), px(4 * w, 4 * h));
                l.input = take(n1 * 3 * fold * fold);
                l.feat = take(n1 * 64);
                l.wide = take(n1 * 192);
                l.start = take(n1 * 64);
                l.tmp = take(n1 * 64);
                l.u1a = take(n2 * 64);
                l.u1b = take(n2 * 64);
                l.u2a = take(n4 * 64);
                l.u2b = take(n4 * 64);
                l.out_len = n4 * 3;
                l.out = take(l.out_len);
            }
        }
        l.total = at;
        l
    }
}

// ----- operations -----------------------------------------------------------------

enum Op<'a> {
    Conv {
        conv: &'a GpuConv,
        act: Act<'static>,
        w: usize,
        h: usize,
        input: usize,
        in_stride: usize,
        out: usize,
        out_stride: usize,
    },
    /// `dst = a * dst + b * src` over `c` channels (`a == 0` ignores `dst`).
    Mix {
        w: usize,
        h: usize,
        c: usize,
        dst: usize,
        dst_stride: usize,
        src: usize,
        src_stride: usize,
        a: f32,
        b: f32,
    },
    /// Nearest-neighbour 2x of a `w x h` map of `c` channels.
    Up {
        w: usize,
        h: usize,
        c: usize,
        src: usize,
        dst: usize,
    },
}

fn ops<'a>(net: &'a GpuNet, l: &Layout, pw: usize, ph: usize) -> Vec<Op<'a>> {
    let conv = |conv: &'a GpuConv, act, (w, h), input, in_stride, out, out_stride| Op::Conv {
        conv,
        act,
        w,
        h,
        input,
        in_stride,
        out,
        out_stride,
    };
    let leaky = Act::Leaky(0.2);
    let mut v = Vec::new();
    match net {
        GpuNet::Compact(convs) => {
            let g = (pw, ph);
            let (mut src, mut stride) = (l.input, 3);
            for (j, c) in convs.iter().enumerate() {
                let last = j == convs.len() - 1;
                let dst = if last {
                    l.out
                } else if j % 2 == 0 {
                    l.a
                } else {
                    l.b
                };
                // PReLU slopes come from the conv's own buffer.
                let act = if last { Act::None } else { Act::PRelu(&[]) };
                v.push(conv(c, act, g, src, stride, dst, c.cout));
                (src, stride) = (dst, c.cout);
            }
        }
        GpuNet::Rrdb {
            first,
            blocks,
            body,
            up1,
            up2,
            hr,
            last,
            fold,
        } => {
            let (w, h) = (pw / fold, ph / fold);
            let g = (w, h);
            let mix = |dst, dst_stride, src, src_stride, a, b| Op::Mix {
                w,
                h,
                c: 64,
                dst,
                dst_stride,
                src,
                src_stride,
                a,
                b,
            };
            v.push(conv(first, Act::None, g, l.input, first.cin, l.feat, 64));
            v.push(mix(l.wide, 192, l.feat, 64, 0.0, 1.0));
            for block in blocks {
                v.push(mix(l.start, 64, l.wide, 192, 0.0, 1.0));
                for dense in block {
                    for (i, c) in dense[..4].iter().enumerate() {
                        v.push(conv(c, leaky, g, l.wide, 192, l.wide + 64 + 32 * i, 192));
                    }
                    v.push(conv(&dense[4], Act::None, g, l.wide, 192, l.tmp, 64));
                    v.push(mix(l.wide, 192, l.tmp, 64, 1.0, 0.2));
                }
                v.push(mix(l.wide, 192, l.start, 64, 0.2, 1.0));
            }
            v.push(conv(body, Act::None, g, l.wide, 192, l.tmp, 64));
            v.push(mix(l.tmp, 64, l.feat, 64, 1.0, 1.0));
            let g2 = (2 * w, 2 * h);
            let g4 = (4 * w, 4 * h);
            v.push(Op::Up {
                w,
                h,
                c: 64,
                src: l.tmp,
                dst: l.u1a,
            });
            v.push(conv(up1, leaky, g2, l.u1a, 64, l.u1b, 64));
            v.push(Op::Up {
                w: g2.0,
                h: g2.1,
                c: 64,
                src: l.u1b,
                dst: l.u2a,
            });
            v.push(conv(up2, leaky, g4, l.u2a, 64, l.u2b, 64));
            v.push(conv(hr, leaky, g4, l.u2b, 64, l.u2a, 64));
            v.push(conv(last, Act::None, g4, l.u2a, 64, l.out, 3));
        }
    }
    v
}

/// The 12 words of an operation's parameter block.
fn params(op: &Op) -> [u32; 12] {
    let u = |v: usize| v as u32;
    match op {
        Op::Conv {
            conv,
            act,
            w,
            h,
            input,
            in_stride,
            out,
            out_stride,
        } => {
            let (code, slope) = match act {
                Act::None => (0, 0.0f32),
                Act::Leaky(s) => (1, *s),
                Act::PRelu(_) => (2, 0.0),
            };
            [
                u(*w),
                u(*h),
                u(conv.cin),
                u(conv.cout),
                u(*input),
                u(*in_stride),
                u(*out),
                u(*out_stride),
                code,
                slope.to_bits(),
                u(conv.cout.next_multiple_of(4)),
                0,
            ]
        }
        Op::Mix {
            w,
            h,
            c,
            dst,
            dst_stride,
            src,
            src_stride,
            a,
            b,
        } => [
            u(*w),
            u(*h),
            u(*c),
            u(*dst),
            u(*dst_stride),
            u(*src),
            u(*src_stride),
            a.to_bits(),
            b.to_bits(),
            0,
            0,
            0,
        ],
        Op::Up { w, h, c, src, dst } => {
            [u(*w), u(*h), u(*c), u(*src), u(*dst), 0, 0, 0, 0, 0, 0, 0]
        }
    }
}

// ----- running ------------------------------------------------------------------

/// The arena size for the largest tile of side `t` (plus overlap).
fn arena_bytes(net: &GpuNet, t: usize) -> u64 {
    Layout::new(net, t, t).total as u64 * 4
}

/// Enlarge `src` (RGB, `w x h`) on the graphics card. `None` when there is no
/// usable card or anything fails, so the caller runs the processor instead.
pub(super) fn upscale(
    net: &Ready,
    src: &[f32],
    w: usize,
    h: usize,
    fixed_tile: Option<usize>,
    pad: usize,
) -> Option<(Vec<f32>, String)> {
    let g = &*gpu()?;
    if *g.broken.lock().ok()? || lost() {
        return None;
    }
    let gnet = net_on_gpu(g, net);
    let fold = match &*gnet {
        GpuNet::Rrdb { fold, .. } => *fold,
        GpuNet::Compact(_) => 1,
    };
    // The largest tile whose maps fit in one binding, at least the CPU's.
    let cap = g.max_binding.min(ARENA_CAP);
    let fits = |t: usize| arena_bytes(&gnet, (t + 2 * pad).next_multiple_of(fold)) <= cap;
    let tile = match fixed_tile {
        Some(t) => Some(t).filter(|&t| fits(t))?,
        None => [1024, 768, 512, 448, 384, 320, 256, 192, 128, 96, 64]
            .into_iter()
            .find(|&t| t <= g.max_tile && fits(t))?,
    };
    let s = net.scale();
    let list = tiles(w, h, tile, pad, fold);
    let biggest = list
        .iter()
        .map(|t| Layout::new(&gnet, t.pw, t.ph))
        .max_by_key(|l| l.total)?;
    let arena = g.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("arena"),
        size: biggest.total as u64 * 4,
        usage: wgpu::BufferUsages::STORAGE
            | wgpu::BufferUsages::COPY_DST
            | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let readback = g.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: biggest.out_len as u64 * 4,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let (ow, oh) = (w * s, h * s);
    let mut out = vec![0f32; ow * oh * 3];
    let guard = g.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    let guard2 = g.device.push_error_scope(wgpu::ErrorFilter::Validation);
    for t in &list {
        let rgb = crop(src, w, h, t);
        let big = match run_tile(g, &gnet, &arena, &readback, &rgb, t.pw, t.ph) {
            Some(b) => b,
            None => {
                fail(g);
                return None;
            }
        };
        place(&mut out, ow, s, t, &big);
    }
    let validation = pollster::block_on(guard2.pop());
    let oom = pollster::block_on(guard.pop());
    if let Some(e) = validation.or(oom) {
        eprintln!("graphics card failed, using the processor: {e}");
        fail(g);
        return None;
    }
    Some((out, g.name.clone()))
}

fn lost() -> bool {
    LOST.load(std::sync::atomic::Ordering::SeqCst)
}

/// Stop using the graphics card for the rest of the session (after a
/// panic inside the graphics driver layer, for instance).
pub(super) fn give_up() {
    note("the graphics driver failed; using the processor");
    LOST.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Floating-point work of one operation, to keep submissions short.
fn flops(op: &Op) -> f64 {
    match op {
        Op::Conv { conv, w, h, .. } => (w * h * conv.cin * conv.cout * 18) as f64,
        _ => 0.0,
    }
}

/// Consecutive runs of operations, each short enough for one submission.
fn batches(list: &[Op]) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let (mut start, mut work) = (0, 0.0);
    for (i, op) in list.iter().enumerate() {
        let f = flops(op);
        if i > start && (work + f > FLOPS_PER_SUBMIT || i - start >= PER_SUBMIT) {
            out.push(start..i);
            start = i;
            work = 0.0;
        }
        work += f;
    }
    if start < list.len() {
        out.push(start..list.len());
    }
    out
}

fn fail(g: &Gpu) {
    note(format!(
        "{} failed while working; using the processor",
        g.name
    ));
    if let Ok(mut b) = g.broken.lock() {
        *b = true;
    }
}

/// Run one padded tile; gives the enlarged RGB tile, like the CPU nets.
fn run_tile(
    g: &Gpu,
    net: &GpuNet,
    arena: &wgpu::Buffer,
    readback: &wgpu::Buffer,
    rgb: &[f32],
    pw: usize,
    ph: usize,
) -> Option<Vec<f32>> {
    let l = Layout::new(net, pw, ph);
    // Input: padded, folded for the 2x model.
    let (input, iw, ih, ic) = match net {
        GpuNet::Rrdb { fold: 2, .. } => (rrdb::unfold(rgb, pw, ph, 2), pw / 2, ph / 2, 12),
        _ => (rgb.to_vec(), pw, ph, 3),
    };
    let padded = super::conv::pad(&input, super::conv::Geo { w: iw, h: ih }, ic);

    let list = ops(net, &l, pw, ph);
    let mut block = Vec::with_capacity(list.len() * PARAM_STRIDE as usize);
    for op in &list {
        let mut p: Vec<u8> = params(op).iter().flat_map(|w| w.to_le_bytes()).collect();
        p.resize(PARAM_STRIDE as usize, 0);
        block.extend_from_slice(&p);
    }
    let pbuf = g
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("params"),
            contents: &block,
            usage: wgpu::BufferUsages::UNIFORM,
        });

    // Borders must read as zero: clear the whole arena, then write the input.
    let mut enc = g.device.create_command_encoder(&Default::default());
    enc.clear_buffer(arena, 0, None);
    g.queue.submit([enc.finish()]);
    g.queue
        .write_buffer(arena, l.input as u64 * 4, &bytes(&padded));

    for range in batches(&list) {
        if lost() {
            return None;
        }
        let first = range.start;
        let chunk = &list[range];
        let mut enc = g.device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_compute_pass(&Default::default());
            for (i, op) in chunk.iter().enumerate() {
                let at = ((first + i) as u64) * PARAM_STRIDE;
                let pb = wgpu::BufferBinding {
                    buffer: &pbuf,
                    offset: at,
                    size: wgpu::BufferSize::new(48),
                };
                match op {
                    Op::Conv { conv, w, h, .. } => {
                        let bg = g.device.create_bind_group(&wgpu::BindGroupDescriptor {
                            label: None,
                            layout: &g.conv_layout,
                            entries: &[
                                entry(0, arena.as_entire_binding()),
                                entry(1, conv.w.as_entire_binding()),
                                entry(2, conv.bias.as_entire_binding()),
                                entry(
                                    3,
                                    conv.slopes.as_ref().unwrap_or(&g.dummy).as_entire_binding(),
                                ),
                                entry(4, wgpu::BindingResource::Buffer(pb)),
                            ],
                        });
                        pass.set_pipeline(&g.conv);
                        pass.set_bind_group(0, &bg, &[]);
                        pass.dispatch_workgroups(
                            w.div_ceil(8) as u32,
                            h.div_ceil(8) as u32,
                            conv.cout.div_ceil(16) as u32,
                        );
                    }
                    Op::Mix { w, h, c, .. } | Op::Up { w, h, c, .. } => {
                        let up = matches!(op, Op::Up { .. });
                        let bg = g.device.create_bind_group(&wgpu::BindGroupDescriptor {
                            label: None,
                            layout: &g.simple_layout,
                            entries: &[
                                entry(0, arena.as_entire_binding()),
                                entry(4, wgpu::BindingResource::Buffer(pb)),
                            ],
                        });
                        let f = if up { 2 } else { 1 };
                        pass.set_pipeline(if up { &g.up } else { &g.mix });
                        pass.set_bind_group(0, &bg, &[]);
                        pass.dispatch_workgroups(
                            (w * f * c).div_ceil(64) as u32,
                            (h * f) as u32,
                            1,
                        );
                    }
                }
            }
        }
        g.queue.submit([enc.finish()]);
    }

    let mut enc = g.device.create_command_encoder(&Default::default());
    enc.copy_buffer_to_buffer(arena, l.out as u64 * 4, readback, 0, l.out_len as u64 * 4);
    g.queue.submit([enc.finish()]);
    let slice = readback.slice(..l.out_len as u64 * 4);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    // A tile takes seconds even on a small integrated GPU; if the driver
    // stalls, give up and let the processor do the photo instead of
    // waiting forever.
    if lost() {
        return None;
    }
    if let Err(e) = g.device.poll(wgpu::PollType::Wait {
        submission_index: None,
        timeout: Some(TILE_TIMEOUT),
    }) {
        eprintln!("graphics card did not finish a tile ({e}), using the processor");
        return None;
    }
    rx.recv().ok()?.ok()?;
    let data: Vec<f32> = {
        let view = slice.get_mapped_range().ok()?;
        view.chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect()
    };
    readback.unmap();

    Some(match net {
        GpuNet::Compact(_) => compact::shuffle(&data, rgb, pw, ph),
        GpuNet::Rrdb { .. } => {
            let (ow, oh) = (pw * 4 / fold_of(net), ph * 4 / fold_of(net));
            let wp = ow + 2;
            let mut out = Vec::with_capacity(ow * oh * 3);
            for y in 0..oh {
                for v in &data[((y + 1) * wp + 1) * 3..][..ow * 3] {
                    out.push(v.clamp(0.0, 1.0));
                }
            }
            out
        }
    })
}

fn fold_of(net: &GpuNet) -> usize {
    match net {
        GpuNet::Rrdb { fold, .. } => *fold,
        GpuNet::Compact(_) => 1,
    }
}

fn entry(binding: u32, resource: wgpu::BindingResource<'_>) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry { binding, resource }
}
