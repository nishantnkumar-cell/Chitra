// Compute shaders for the AI upscaler; see gpu.rs. Feature maps are padded
// HWC regions of one storage buffer: a `w x h` map is (w + 2) x (h + 2)
// pixels with a zero border, `stride` floats per pixel. Shaders only write
// the inside of a map, so borders stay as cleared.

// One parameter block of 12 words; field meanings differ per shader.
struct Params {
    w: u32,
    h: u32,
    c0: u32,
    c1: u32,
    u4: u32,
    u5: u32,
    u6: u32,
    u7: u32,
    u8: u32,
    u9: u32,
    u10: u32,
    u11: u32,
}

@group(0) @binding(0) var<storage, read_write> arena: array<f32>;
@group(0) @binding(1) var<storage, read> wts: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read> bias: array<f32>;
@group(0) @binding(3) var<storage, read> slopes: array<f32>;
@group(0) @binding(4) var<uniform> p: Params;

// ----- 3x3 convolution ----------------------------------------------------------
// Params: w, h, cin, cout, in_base, in_stride, out_base, out_stride,
// act (0 none, 1 leaky, 2 PReLU), leaky slope (f32 bits), cout rounded up to 4.
// A workgroup makes 8 x 8 pixels x 16 output channels; input channels are
// taken 16 at a time through workgroup memory.

const TS: u32 = 8u;
const PATCH: u32 = 10u;
const CI: u32 = 16u;
const CO: u32 = 16u;

var<workgroup> tin: array<f32, 1600>;       // [PATCH][PATCH][CI]
var<workgroup> tw: array<vec4<f32>, 576>;   // [9][CI][CO / 4]

@compute @workgroup_size(8, 8, 1)
fn conv(
    @builtin(workgroup_id) wg: vec3<u32>,
    @builtin(local_invocation_id) lid: vec3<u32>,
    @builtin(local_invocation_index) li: u32,
) {
    let w = p.w;
    let h = p.h;
    let cin = p.c0;
    let cout = p.c1;
    let in_base = p.u4;
    let in_stride = p.u5;
    let out_base = p.u6;
    let out_stride = p.u7;
    let act = p.u8;
    let leaky = bitcast<f32>(p.u9);
    let c4 = p.u10 / 4u;
    let wp = w + 2u;
    let hp = h + 2u;
    // Padded coordinates: the patch starts one pixel up and left of the block.
    let ox = wg.x * TS;
    let oy = wg.y * TS;
    let x = ox + lid.x + 1u;
    let y = oy + lid.y + 1u;
    let co0 = wg.z * CO;

    var a0 = vec4<f32>(0.0);
    var a1 = vec4<f32>(0.0);
    var a2 = vec4<f32>(0.0);
    var a3 = vec4<f32>(0.0);

    for (var cb = 0u; cb < cin; cb += CI) {
        for (var e = li; e < PATCH * PATCH * CI; e += 64u) {
            let ci = e % CI;
            let pix = e / CI;
            let gx = ox + pix % PATCH;
            let gy = oy + pix / PATCH;
            var v = 0.0;
            if (gx < wp && gy < hp && cb + ci < cin) {
                v = arena[in_base + (gy * wp + gx) * in_stride + cb + ci];
            }
            tin[e] = v;
        }
        for (var e = li; e < 9u * CI * 4u; e += 64u) {
            let q = e % 4u;
            let ci = (e / 4u) % CI;
            let k = e / (4u * CI);
            let col = co0 / 4u + q;
            var v = vec4<f32>(0.0);
            if (cb + ci < cin && col < c4) {
                v = wts[(k * cin + cb + ci) * c4 + col];
            }
            tw[e] = v;
        }
        workgroupBarrier();
        for (var k = 0u; k < 9u; k++) {
            let tb = ((lid.y + k / 3u) * PATCH + lid.x + k % 3u) * CI;
            let wb = k * CI * 4u;
            for (var ci = 0u; ci < CI; ci++) {
                let v = tin[tb + ci];
                let o = wb + ci * 4u;
                a0 += v * tw[o];
                a1 += v * tw[o + 1u];
                a2 += v * tw[o + 2u];
                a3 += v * tw[o + 3u];
            }
        }
        workgroupBarrier();
    }

    if (x > w || y > h) {
        return;
    }
    let acc = array<vec4<f32>, 4>(a0, a1, a2, a3);
    let dst = out_base + (y * wp + x) * out_stride;
    for (var j = 0u; j < CO; j++) {
        let co = co0 + j;
        if (co >= cout) {
            break;
        }
        var v = acc[j / 4u][j % 4u] + bias[co];
        if (v < 0.0) {
            if (act == 1u) {
                v *= leaky;
            } else if (act == 2u) {
                v *= slopes[co];
            }
        }
        arena[dst + co] = v;
    }
}

// ----- dst = a * dst + b * src ------------------------------------------------------
// Params: w, h, c, dst_base, dst_stride, src_base, src_stride, a, b (f32 bits).

@compute @workgroup_size(64, 1, 1)
fn mix(@builtin(global_invocation_id) id: vec3<u32>) {
    let c = p.c0;
    if (id.x >= p.w * c || id.y >= p.h) {
        return;
    }
    let pix = (id.y + 1u) * (p.w + 2u) + id.x / c + 1u;
    let ch = id.x % c;
    let a = bitcast<f32>(p.u7);
    let b = bitcast<f32>(p.u8);
    let s = arena[p.u5 + pix * p.u6 + ch];
    let di = p.c1 + pix * p.u4 + ch;
    var d = 0.0;
    if (a != 0.0) {
        d = a * arena[di];
    }
    arena[di] = d + b * s;
}

// ----- nearest-neighbour 2x ------------------------------------------------------------
// Params: w, h (of the source), c, src_base, dst_base; both maps have c
// channels per pixel.

@compute @workgroup_size(64, 1, 1)
fn up(@builtin(global_invocation_id) id: vec3<u32>) {
    let c = p.c0;
    let ow = p.w * 2u;
    if (id.x >= ow * c || id.y >= p.h * 2u) {
        return;
    }
    let bx = id.x / c;
    let ch = id.x % c;
    let by = id.y;
    let src = ((by / 2u + 1u) * (p.w + 2u) + bx / 2u + 1u) * c + ch;
    let dst = ((by + 1u) * (ow + 2u) + bx + 1u) * c + ch;
    arena[p.u4 + dst] = arena[p.c1 + src];
}
