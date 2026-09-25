// Shared by every scene. The loader prepends this file to each scene's WGSL, so scenes get
// the `Frame` uniform, the textures, the fullscreen vertex stage and the helpers below.
//
// Layout mirrors `onset_render::uniforms::FrameUniforms` field for field (std140):
// every scalar is an f32 so packing is predictable; arrays are vec4 groups.
//
// Scenes render into an HDR target: values above 1.0 bloom. The post pass tonemaps, adds
// bloom, flashes, inversions, hue shifts, shake, glitch and grain on top, so scenes only
// draw the picture.

struct Frame {
    resolution: vec2<f32>,      // render size in pixels
    time: f32,                  // seconds since start
    playhead: f32,              // seconds into the track

    beat_phase: f32,            // 0..1 within the current beat
    bar_phase: f32,             // 0..1 within the current 4-beat bar
    phrase_phase: f32,          // 0..1 within the current phrase
    intensity: f32,             // director output 0..1 (calm breakdown .. full drop)

    bpm: f32,                   // tempo being played
    playing: f32,               // 1 when playing
    phrase_kind: f32,           // 0 none, 1 intro, 2 verse, 3 bridge, 4 chorus, 5 outro, 6 up, 7 down
    next_phrase_kind: f32,      // same encoding

    beats_to_next: f32,         // beats until the next phrase, -1 unknown
    drop_countdown: f32,        // beats until the next drop, -1 unknown
    rms: f32,                   // raw loudness
    onset: f32,                 // 1 on an onset hop

    bands: array<vec4<f32>, 6>, // 24 raw log-spaced bands, 30 Hz .. 16 kHz
    theme: array<vec4<f32>, 5>, // background, text, accent1, accent2, accent3 (linear RGB, from the cover)

    levels: array<vec4<f32>, 6>, // the 24 bands normalised 0..1 against their own recent peak
    groups: vec4<f32>,          // sub, bass, low-mid, mid (0..1)
    groups2: vec4<f32>,         // high-mid, treble, loudness, brightness
    hits: vec4<f32>,            // kick, snare, hat (decaying 0..1), onset
    fx0: vec4<f32>,             // drop_hit, phrase_hit, cue_hit, track_hit (decaying 0..1)
    fx1: vec4<f32>,             // tension (pre-drop 0..1), beat_count, bar_count, seed (0..1 per track)
    fx2: vec4<f32>,             // hue shift (turns), reactivity (0..2), trails (0..1), quality (0 low .. 3 ultra)
    cue: vec4<f32>,             // colour of the last cue passed (rgb), cue_hit
    vibe: vec4<f32>,            // energy, darkness, rekordbox mood (0 none, 1 low, 2 mid, 3 high), history row (0..1)
    stems: vec4<f32>,           // rekordbox's analysis at the playhead: low, mid, high, vocal (0..1)
    scene: vec4<f32>,           // the DJ's tweaks for this scene: speed, intensity, colour shift (turns), -
};

@group(0) @binding(0) var<uniform> frame: Frame;
@group(0) @binding(1) var samp: sampler;
// The previous frame's scene output (HDR), for trails and feedback effects.
@group(0) @binding(2) var prev_tex: texture_2d<f32>;
// Spectrum history: x = band (0..23 levels, 24..29 groups, 30 kick, 31 snare), y = time
// (rows wrap; `history(u, age)` handles it). Values 0..1.
@group(0) @binding(3) var history_tex: texture_2d<f32>;
// The current track's cover art (placeholder when none).
@group(0) @binding(4) var art_tex: texture_2d<f32>;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// One triangle covering the screen; uv runs 0..1 with y down like an image.
@vertex
fn vs_fullscreen(@builtin(vertex_index) i: u32) -> VsOut {
    var out: VsOut;
    let x = f32(i32(i & 1u) * 4 - 1);
    let y = f32(i32(i & 2u) * 2 - 1);
    out.pos = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>(x, -y) * 0.5 + vec2<f32>(0.5, 0.5);
    return out;
}

const PI: f32 = 3.14159265;
const TAU: f32 = 6.2831853;

// ---------------------------------------------------------------- audio

fn band(i: u32) -> f32 {
    return frame.bands[i / 4u][i % 4u];
}

// Normalised level of band i (0..23), 0..1 at any volume, scaled by reactivity.
fn lvl(i: u32) -> f32 {
    return clamp(frame.levels[min(i, 23u) / 4u][min(i, 23u) % 4u] * reactivity(), 0.0, 1.5);
}

// Level at a continuous position t in 0..1 across the spectrum (interpolated).
fn spectrum(t: f32) -> f32 {
    let x = clamp(t, 0.0, 1.0) * 23.0;
    let i = u32(floor(x));
    return mix(lvl(i), lvl(min(i + 1u, 23u)), fract(x));
}

fn reactivity() -> f32 { return frame.fx2.y * frame.scene.y; }
fn sub() -> f32 { return frame.groups.x * reactivity(); }
fn bass() -> f32 { return frame.groups.y * reactivity(); }
fn lowmid() -> f32 { return frame.groups.z * reactivity(); }
fn mid() -> f32 { return frame.groups.w * reactivity(); }
fn highmid() -> f32 { return frame.groups2.x * reactivity(); }
fn treble() -> f32 { return frame.groups2.y * reactivity(); }
fn loudness() -> f32 { return frame.groups2.z; }
fn brightness() -> f32 { return frame.groups2.w; }
fn kick() -> f32 { return frame.hits.x * reactivity(); }
fn snare() -> f32 { return frame.hits.y * reactivity(); }
fn hat() -> f32 { return frame.hits.z * reactivity(); }
// rekordbox's own analysis of the loaded track at the playhead (no audio latency):
// the 3-band waveform and the vocal detection. `vocal()` is gated by the live mids, so a
// vocal the DJ has cut with the stems or EQ fades out.
fn track_low() -> f32 { return frame.stems.x * reactivity(); }
fn track_mid() -> f32 { return frame.stems.y * reactivity(); }
fn track_high() -> f32 { return frame.stems.z * reactivity(); }
fn vocal() -> f32 { return frame.stems.w * smoothstep(0.05, 0.35, frame.groups.w) * reactivity(); }

// ---------------------------------------------------------------- show

fn drop_hit() -> f32 { return frame.fx0.x; }
fn phrase_hit() -> f32 { return frame.fx0.y; }
fn cue_hit() -> f32 { return frame.fx0.z; }
fn track_hit() -> f32 { return frame.fx0.w; }
fn tension() -> f32 { return frame.fx1.x; }
fn beat_count() -> f32 { return frame.fx1.y; }
fn bar_count() -> f32 { return frame.fx1.z; }
fn seed() -> f32 { return frame.fx1.w; }
fn hue_shift() -> f32 { return frame.fx2.x; }
fn trails() -> f32 { return frame.fx2.z; }
// 0 low, 1 medium, 2 high, 3 ultra: scale loop counts with this.
fn quality() -> f32 { return frame.fx2.w; }
fn energy() -> f32 { return frame.vibe.x; }
fn darkness() -> f32 { return frame.vibe.y; }
fn is_chorus() -> f32 { return select(0.0, 1.0, frame.phrase_kind == 4.0); }
fn is_breakdown() -> f32 {
    return select(0.0, 1.0, frame.phrase_kind == 3.0 || frame.phrase_kind == 5.0 || frame.phrase_kind == 7.0);
}

// Sharp-then-decaying pulse from a 0..1 phase: 1 at the beat, fading over the beat.
fn beat_pulse(phase: f32, sharpness: f32) -> f32 {
    return pow(clamp(1.0 - phase, 0.0, 1.0), sharpness);
}

// A time that runs with the music: seconds while stopped, beats (scaled) while playing, so
// motion locks to the tempo and speeds up with the pitch fader.
fn music_time() -> f32 {
    return select(frame.time * 0.5, beat_count() * 0.5, frame.playing > 0.5 && frame.bpm > 1.0) * frame.scene.x;
}

// ---------------------------------------------------------------- textures

fn prev_frame(uv: vec2<f32>) -> vec3<f32> {
    return textureSampleLevel(prev_tex, samp, clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0)), 0.0).rgb;
}

// Spectrum history at spectrum position u (0..1) and age (0 = now .. 1 = oldest, ~2 s).
fn history(u: f32, age: f32) -> f32 {
    let row = fract(frame.vibe.w - clamp(age, 0.0, 0.999));
    let x = (clamp(u, 0.0, 1.0) * 23.0 + 0.5) / 32.0;
    return textureSampleLevel(history_tex, samp, vec2<f32>(x, row), 0.0).r * reactivity();
}

fn history_kick(age: f32) -> f32 {
    let row = fract(frame.vibe.w - clamp(age, 0.0, 0.999));
    return textureSampleLevel(history_tex, samp, vec2<f32>(30.5 / 32.0, row), 0.0).r;
}

fn artwork(uv: vec2<f32>) -> vec3<f32> {
    return textureSampleLevel(art_tex, samp, fract(uv), 0.0).rgb;
}

// ---------------------------------------------------------------- colour

fn theme(i: u32) -> vec3<f32> {
    return frame.theme[i].rgb;
}

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

// Rotates hue by `turns` around the grey axis (keeps luminance roughly).
fn hue_rotate(c: vec3<f32>, turns: f32) -> vec3<f32> {
    let a = turns * TAU;
    let k = vec3<f32>(0.57735);
    let cs = cos(a);
    return c * cs + cross(k, c) * sin(a) + k * dot(k, c) * (1.0 - cs);
}

fn hsv(h: f32, s: f32, v: f32) -> vec3<f32> {
    let k = vec3<f32>(1.0, 2.0 / 3.0, 1.0 / 3.0);
    let p = abs(fract(vec3<f32>(h) + k) * 6.0 - 3.0);
    return v * mix(vec3<f32>(1.0), clamp(p - 1.0, vec3<f32>(0.0), vec3<f32>(1.0)), s);
}

// A colour from the track's palette: t cycles through the three accents smoothly.
fn palette(t: f32) -> vec3<f32> {
    let x = fract(t + frame.scene.z) * 3.0;
    let a = theme(2u);
    let b = theme(3u);
    let c = theme(4u);
    var col: vec3<f32>;
    if (x < 1.0) {
        col = mix(a, b, smoothstep(0.0, 1.0, x));
    } else if (x < 2.0) {
        col = mix(b, c, smoothstep(0.0, 1.0, x - 1.0));
    } else {
        col = mix(c, a, smoothstep(0.0, 1.0, x - 2.0));
    }
    // Keep accents vivid even when the cover is muted.
    let l = luma(col);
    return max(mix(vec3<f32>(l), col, 1.6), vec3<f32>(0.0));
}

// Inigo Quilez's cosine palette, for scenes that want a rainbow sweep.
fn cospal(t: f32, a: vec3<f32>, b: vec3<f32>, c: vec3<f32>, d: vec3<f32>) -> vec3<f32> {
    return a + b * cos(TAU * (c * t + d));
}

// ---------------------------------------------------------------- space

// Aspect-corrected coordinates centred on the screen, y up, x in [-aspect, aspect].
fn centred(uv: vec2<f32>) -> vec2<f32> {
    let aspect = frame.resolution.x / max(frame.resolution.y, 1.0);
    return vec2<f32>((uv.x - 0.5) * 2.0 * aspect, (0.5 - uv.y) * 2.0);
}

fn rot2(a: f32) -> mat2x2<f32> {
    let c = cos(a);
    let s = sin(a);
    return mat2x2<f32>(c, s, -s, c);
}

// ---------------------------------------------------------------- noise

// Integer hash of the cell containing p. Integer maths keeps the noise stable however far
// the coordinates drift over a long set; a fractional hash collapses to a few values once
// f32 precision runs out (around 10^4).
fn hash21(p: vec2<f32>) -> f32 {
    let c = vec2<i32>(floor(p));
    var n = bitcast<u32>(c.x) * 1597334677u ^ bitcast<u32>(c.y) * 3812015801u;
    n = (n ^ (n >> 16u)) * 0x45d9f3bu;
    n = (n ^ (n >> 16u)) * 0x45d9f3bu;
    n = n ^ (n >> 16u);
    return f32(n) / 4294967295.0;
}

fn hash31(p: vec3<f32>) -> f32 {
    let c = vec3<i32>(floor(p));
    var n = bitcast<u32>(c.x) * 1597334677u ^ bitcast<u32>(c.y) * 3812015801u ^ bitcast<u32>(c.z) * 2798796415u;
    n = (n ^ (n >> 16u)) * 0x45d9f3bu;
    n = (n ^ (n >> 16u)) * 0x45d9f3bu;
    n = n ^ (n >> 16u);
    return f32(n) / 4294967295.0;
}

fn hash22(p: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(hash21(p), hash21(p + vec2<f32>(19.19, 73.7)));
}

fn noise2(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let a = hash21(i);
    let b = hash21(i + vec2<f32>(1.0, 0.0));
    let c = hash21(i + vec2<f32>(0.0, 1.0));
    let d = hash21(i + vec2<f32>(1.0, 1.0));
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

fn noise3(p: vec3<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let n000 = hash31(i);
    let n100 = hash31(i + vec3<f32>(1.0, 0.0, 0.0));
    let n010 = hash31(i + vec3<f32>(0.0, 1.0, 0.0));
    let n110 = hash31(i + vec3<f32>(1.0, 1.0, 0.0));
    let n001 = hash31(i + vec3<f32>(0.0, 0.0, 1.0));
    let n101 = hash31(i + vec3<f32>(1.0, 0.0, 1.0));
    let n011 = hash31(i + vec3<f32>(0.0, 1.0, 1.0));
    let n111 = hash31(i + vec3<f32>(1.0, 1.0, 1.0));
    return mix(
        mix(mix(n000, n100, u.x), mix(n010, n110, u.x), u.y),
        mix(mix(n001, n101, u.x), mix(n011, n111, u.x), u.y),
        u.z,
    );
}

fn fbm(p_in: vec2<f32>) -> f32 {
    var p = p_in;
    var v = 0.0;
    var a = 0.5;
    for (var k = 0; k < 4; k = k + 1) {
        v = v + a * noise2(p);
        p = p * 2.03 + vec2<f32>(17.1, 9.2);
        a = a * 0.5;
    }
    return v;
}

fn fbm3(p_in: vec3<f32>, octaves: i32) -> f32 {
    var p = p_in;
    var v = 0.0;
    var a = 0.5;
    for (var k = 0; k < octaves; k = k + 1) {
        v = v + a * noise3(p);
        p = p * 2.02 + vec3<f32>(17.1, 9.2, 4.7);
        a = a * 0.5;
    }
    return v;
}

// ---------------------------------------------------------------- shapes

fn sd_box(p: vec3<f32>, b: vec3<f32>) -> f32 {
    let q = abs(p) - b;
    return length(max(q, vec3<f32>(0.0))) + min(max(q.x, max(q.y, q.z)), 0.0);
}

// Distance to the edges of a box (a wireframe cube), edge thickness e.
fn sd_box_frame(p_in: vec3<f32>, b: vec3<f32>, e: f32) -> f32 {
    let p = abs(p_in) - b;
    let q = abs(p + e) - e;
    return min(min(
        length(max(vec3<f32>(p.x, q.y, q.z), vec3<f32>(0.0))) + min(max(p.x, max(q.y, q.z)), 0.0),
        length(max(vec3<f32>(q.x, p.y, q.z), vec3<f32>(0.0))) + min(max(q.x, max(p.y, q.z)), 0.0)),
        length(max(vec3<f32>(q.x, q.y, p.z), vec3<f32>(0.0))) + min(max(q.x, max(q.y, p.z)), 0.0));
}

fn sd_octahedron(p_in: vec3<f32>, s: f32) -> f32 {
    let p = abs(p_in);
    return (p.x + p.y + p.z - s) * 0.57735027;
}

fn sd_torus(p: vec3<f32>, t: vec2<f32>) -> f32 {
    let q = vec2<f32>(length(p.xz) - t.x, p.y);
    return length(q) - t.y;
}

fn smin(a: f32, b: f32, k: f32) -> f32 {
    let h = clamp(0.5 + 0.5 * (b - a) / k, 0.0, 1.0);
    return mix(b, a, h) - k * h * (1.0 - h);
}

// Distance from p to the segment a..b.
fn sd_segment(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let pa = p - a;
    let ba = b - a;
    let h = clamp(dot(pa, ba) / max(dot(ba, ba), 1e-6), 0.0, 1.0);
    return length(pa - ba * h);
}
