// The stage's own passes: scene transitions, bloom and the final composite. Scenes draw an
// HDR picture; this file turns it into the frame the audience sees.
//
// Layout mirrors `onset_render::stage::PostUniforms` (all vec4, std140).

struct Post {
    res_texel: vec4<f32>,   // output width, height, 1/width, 1/height
    a: vec4<f32>,           // time, flash, invert, hue shift (turns)
    flash_col: vec4<f32>,   // flash colour rgb, strobe
    motion: vec4<f32>,      // shake x, shake y (uv units), zoom punch, glitch
    look: vec4<f32>,        // bloom strength, grain, vignette, chromatic aberration
    tone: vec4<f32>,        // exposure, seed, bloom threshold, blackout
    trans: vec4<f32>,       // transition progress 0..1, kind, emphasis, tension
};

@group(0) @binding(0) var<uniform> post: Post;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var src_a: texture_2d<f32>;
@group(0) @binding(3) var src_b: texture_2d<f32>;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_fullscreen(@builtin(vertex_index) i: u32) -> VsOut {
    var out: VsOut;
    let x = f32(i32(i & 1u) * 4 - 1);
    let y = f32(i32(i & 2u) * 2 - 1);
    out.pos = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>(x, -y) * 0.5 + vec2<f32>(0.5, 0.5);
    return out;
}

fn hash12(p: vec2<f32>) -> f32 {
    let c = vec2<i32>(floor(p));
    var n = bitcast<u32>(c.x) * 1597334677u ^ bitcast<u32>(c.y) * 3812015801u;
    n = (n ^ (n >> 16u)) * 0x45d9f3bu;
    n = (n ^ (n >> 16u)) * 0x45d9f3bu;
    n = n ^ (n >> 16u);
    return f32(n) / 4294967295.0;
}

fn sa(uv: vec2<f32>) -> vec3<f32> {
    return textureSampleLevel(src_a, samp, uv, 0.0).rgb;
}

fn sb(uv: vec2<f32>) -> vec3<f32> {
    return textureSampleLevel(src_b, samp, uv, 0.0).rgb;
}

fn texel_a() -> vec2<f32> {
    return 1.0 / vec2<f32>(textureDimensions(src_a));
}

// ------------------------------------------------------------------ transitions
// src_a = the outgoing scene, src_b = the incoming one.

@fragment
fn fs_transition(in: VsOut) -> @location(0) vec4<f32> {
    let t = clamp(post.trans.x, 0.0, 1.0);
    let kind = u32(post.trans.y + 0.5);
    let e = smoothstep(0.0, 1.0, t);
    var col: vec3<f32>;
    switch kind {
        case 1u: {
            // Flash cut: bleach to white, swap at the peak, come back.
            let peak = 1.0 - abs(t * 2.0 - 1.0);
            let base = select(sa(in.uv), sb(in.uv), t > 0.5);
            col = base + vec3<f32>(peak * peak * 4.0);
        }
        case 2u: {
            // Zoom blur: both scenes smear towards the centre while they cross.
            let dir = in.uv - vec2<f32>(0.5);
            let amount = sin(t * 3.14159) * 0.25;
            var acc_a = vec3<f32>(0.0);
            var acc_b = vec3<f32>(0.0);
            for (var i = 0; i < 12; i = i + 1) {
                let k = 1.0 - amount * f32(i) / 12.0;
                acc_a = acc_a + sa(vec2<f32>(0.5) + dir * k);
                acc_b = acc_b + sb(vec2<f32>(0.5) + dir * k);
            }
            col = mix(acc_a, acc_b, e) / 12.0;
        }
        case 3u: {
            // Glitch: blocks of the new scene tear in, offset and colour-split.
            let block = floor(in.uv * vec2<f32>(16.0, 24.0));
            let r = hash12(block + floor(post.a.x * 20.0));
            let on = r < t * 1.2 - 0.1;
            let shift = (hash12(block.yx + 7.0) - 0.5) * 0.08 * sin(t * 3.14159);
            let uv = in.uv + vec2<f32>(shift, 0.0);
            let a = vec3<f32>(sa(uv + vec2<f32>(0.006, 0.0)).r, sa(uv).g, sa(uv - vec2<f32>(0.006, 0.0)).b);
            let b = vec3<f32>(sb(uv + vec2<f32>(0.006, 0.0)).r, sb(uv).g, sb(uv - vec2<f32>(0.006, 0.0)).b);
            col = select(a, b, on || t > 0.98);
        }
        case 4u: {
            // Wipe: a soft diagonal edge with a glowing seam.
            let d = (in.uv.x * 0.8 + in.uv.y * 0.2) - (t * 1.4 - 0.2);
            let m = smoothstep(0.04, -0.04, d);
            let seam = exp(-abs(d) * 60.0) * sin(t * 3.14159) * 3.0;
            col = mix(sa(in.uv), sb(in.uv), m) + vec3<f32>(seam);
        }
        default: {
            col = mix(sa(in.uv), sb(in.uv), e);
        }
    }
    return vec4<f32>(col, 1.0);
}

// ------------------------------------------------------------------ bloom

// Bright pass with a soft knee, downsampling 2x with a 4-tap box.
@fragment
fn fs_bright(in: VsOut) -> @location(0) vec4<f32> {
    let o = texel_a() * 0.5;
    let c = (sa(in.uv + vec2<f32>(-o.x, -o.y)) + sa(in.uv + vec2<f32>(o.x, -o.y))
        + sa(in.uv + vec2<f32>(-o.x, o.y)) + sa(in.uv + vec2<f32>(o.x, o.y))) * 0.25;
    let threshold = post.tone.z;
    let knee = threshold * 0.5;
    let br = max(c.r, max(c.g, c.b));
    let soft = clamp(br - threshold + knee, 0.0, 2.0 * knee);
    let contrib = max(soft * soft / (4.0 * knee + 1e-4), br - threshold) / max(br, 1e-4);
    return vec4<f32>(min(c * max(contrib, 0.0), vec3<f32>(64.0)), 1.0);
}

// Dual-filter (Kawase) downsample.
@fragment
fn fs_down(in: VsOut) -> @location(0) vec4<f32> {
    let o = texel_a();
    var c = sa(in.uv) * 4.0;
    c = c + sa(in.uv + vec2<f32>(-o.x, -o.y)) + sa(in.uv + vec2<f32>(o.x, -o.y));
    c = c + sa(in.uv + vec2<f32>(-o.x, o.y)) + sa(in.uv + vec2<f32>(o.x, o.y));
    return vec4<f32>(c / 8.0, 1.0);
}

// Dual-filter upsample, blended additively onto the larger level.
@fragment
fn fs_up(in: VsOut) -> @location(0) vec4<f32> {
    let o = texel_a();
    var c = sa(in.uv + vec2<f32>(-2.0 * o.x, 0.0)) + sa(in.uv + vec2<f32>(2.0 * o.x, 0.0));
    c = c + sa(in.uv + vec2<f32>(0.0, -2.0 * o.y)) + sa(in.uv + vec2<f32>(0.0, 2.0 * o.y));
    c = c + (sa(in.uv + vec2<f32>(-o.x, -o.y)) + sa(in.uv + vec2<f32>(o.x, -o.y))
        + sa(in.uv + vec2<f32>(-o.x, o.y)) + sa(in.uv + vec2<f32>(o.x, o.y))) * 2.0;
    return vec4<f32>(c / 12.0 * 0.85, 1.0);
}

// ------------------------------------------------------------------ composite

fn aces(x: vec3<f32>) -> vec3<f32> {
    let a = 2.51;
    let b = 0.03;
    let c = 2.43;
    let d = 0.59;
    let e = 0.14;
    return clamp((x * (a * x + b)) / (x * (c * x + d) + e), vec3<f32>(0.0), vec3<f32>(1.0));
}

fn hue_rotate(c: vec3<f32>, turns: f32) -> vec3<f32> {
    let a = turns * 6.2831853;
    let k = vec3<f32>(0.57735);
    let cs = cos(a);
    return c * cs + cross(k, c) * sin(a) + k * dot(k, c) * (1.0 - cs);
}

// src_a = the scene (HDR, render size), src_b = bloom (half size).
@fragment
fn fs_composite(in: VsOut) -> @location(0) vec4<f32> {
    if (post.tone.w > 0.5) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    if (post.trans.z > 0.5) {
        // Passthrough: the scene as drawn, only resampled to the output size.
        return vec4<f32>(clamp(sa(in.uv), vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
    }
    let time = post.a.x;
    let glitch = post.motion.w;

    // Camera: zoom punch about the centre, then shake.
    var uv = (in.uv - vec2<f32>(0.5)) / (1.0 + post.motion.z) + vec2<f32>(0.5);
    uv = uv + post.motion.xy;

    // Glitch: horizontal slices jump sideways for a few frames.
    let slice = floor(uv.y * 48.0);
    let tick = floor(time * 24.0);
    let r = hash12(vec2<f32>(slice, tick));
    if (r < glitch * 0.35) {
        uv.x = uv.x + (hash12(vec2<f32>(tick, slice)) - 0.5) * 0.12 * glitch;
    }

    // Chromatic aberration grows towards the edges and with glitches.
    let dir = uv - vec2<f32>(0.5);
    let ca = post.look.w * (0.4 + dot(dir, dir) * 2.0) + glitch * 0.012;
    var col = vec3<f32>(
        sa(uv + dir * ca).r,
        sa(uv).g,
        sa(uv - dir * ca).b,
    );

    col = col + sb(uv) * post.look.x;
    col = col * post.tone.x;
    col = aces(col);

    col = clamp(hue_rotate(col, post.a.w), vec3<f32>(0.0), vec3<f32>(1.0));
    col = mix(col, vec3<f32>(1.0) - col, clamp(post.a.z, 0.0, 1.0));

    // Flash (tinted by the cue colour) and strobe.
    col = col + post.flash_col.rgb * post.a.y * 0.85;
    col = mix(col, vec3<f32>(1.0), post.flash_col.w * 0.9);

    // Vignette, darker as tension builds before a drop.
    let v = dot(dir, dir);
    col = col * (1.0 - clamp(v * (post.look.z + post.trans.w * 1.2), 0.0, 1.0));

    // Film grain, changing every frame.
    let g = hash12(in.pos.xy + vec2<f32>(post.tone.y * 1000.0, floor(time * 60.0) * 17.0)) - 0.5;
    col = col + vec3<f32>(g * post.look.y * 0.12);

    return vec4<f32>(clamp(col, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
