// Liquid: a chrome surface reflecting a coloured studio. Bass makes the swell, mids fold
// it, the highs add a fine shimmer, the kick drops a ripple in the middle, and the drop
// floods the reflections with light.

fn height(p: vec2<f32>, t: f32) -> f32 {
    let q = vec2<f32>(fbm(p * 0.9 + vec2<f32>(t * 0.3, 0.0)), fbm(p * 0.9 + vec2<f32>(3.2, t * 0.25)));
    var h = fbm(p * 1.2 + q * (1.2 + 1.2 * mid()) + vec2<f32>(-t * 0.2, t * 0.1));
    h = h * (0.6 + 0.8 * bass());
    let r = length(p);
    h = h + 0.08 * kick() * sin(r * 14.0 - frame.beat_phase * 20.0) * exp(-r * 1.2);
    h = h + 0.004 * treble() * noise2(p * 30.0 + t * 3.0);
    return h;
}

// A studio of soft coloured light panels and a bright key light.
fn environment(d: vec3<f32>) -> vec3<f32> {
    // Mostly dark, with bright strip lights: chrome needs contrast to read as chrome.
    let horizon = smoothstep(-0.2, 0.9, d.y);
    // A broad sky gradient gives the swells their shape; the strips give the sparkle.
    var c = mix(palette(0.6 + seed()) * 0.02, palette(0.1 + seed()) * 0.8, horizon * horizon);
    let a = atan2(d.x, d.z);
    let strips = smoothstep(0.75, 0.97, sin(a * 3.0 + hue_shift() * 3.0 + d.y * 2.0));
    c = c + palette(0.0 + seed()) * strips * smoothstep(-0.1, 0.5, d.y) * 2.2;
    let rim = smoothstep(0.95, 0.99, sin(d.y * 9.0 - music_time() * 0.3));
    c = c + palette(0.35 + seed()) * rim * 1.2;
    let key = pow(max(dot(d, normalize(vec3<f32>(0.3, 0.8, 0.5))), 0.0), 60.0);
    c = c + vec3<f32>(key) * (3.0 + 4.0 * drop_hit());
    return c;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let uv = centred(in.uv);
    let t = music_time() * 0.4;
    let p = uv * 0.7;
    let e = 0.006;
    let h = height(p, t);
    let hx = height(p + vec2<f32>(e, 0.0), t);
    let hy = height(p + vec2<f32>(0.0, e), t);
    let n = normalize(vec3<f32>(-(hx - h) / e * 0.3, -(hy - h) / e * 0.3, 1.0));
    let view = normalize(vec3<f32>(uv * 0.3, -1.0));
    let r = reflect(view, n);
    // Seen at a slant: tilt the reflection so the swells catch the sky above the horizon.
    let tilt = 1.0;
    let ry = r.y * cos(tilt) + r.z * sin(tilt);
    let rz = -r.y * sin(tilt) + r.z * cos(tilt);
    let env = environment(normalize(vec3<f32>(r.x, ry, rz)));
    let fres = 0.25 + 0.75 * pow(1.0 - max(dot(-view, n), 0.0), 4.0);
    var col = env * fres * (0.7 + 0.6 * loudness() + 0.3 * vocal());
    // Liquid depth: darker in the troughs, tinted.
    col = col + palette(0.15 + seed()) * 0.02 * smoothstep(0.7, 0.2, h);
    col = col * (1.0 - 0.35 * darkness() * (1.0 - frame.intensity));
    return vec4<f32>(col, 1.0);
}
