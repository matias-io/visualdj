// Mandala: a psychedelic kaleidoscope of fine flowing contour lines, like the big festival
// screens. Bass swells the rings, mids warp the flow, highs sharpen and multiply the
// lines, the kick sends a ring outward, and the drop doubles the segments. The previous
// frame is pulled towards the centre for an endless-zoom trail.

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let uv = centred(in.uv);
    let t = music_time() * 0.35;
    let r = length(uv);
    var a = atan2(uv.y, uv.x);

    // Kaleidoscope: fold the angle into mirrored segments.
    let base_segments = 6.0 + 2.0 * floor(energy() * 3.0 + seed() * 2.0);
    let segments = base_segments * (1.0 + step(0.35, drop_hit()));
    let seg = TAU / segments;
    a = abs(((a + t * 0.25) % seg + seg) % seg - seg * 0.5);
    let p = vec2<f32>(cos(a), sin(a)) * r;

    // Domain-warped flow field.
    let warp = 0.6 + 0.9 * mid();
    let q = vec2<f32>(fbm(p * 1.6 + vec2<f32>(t, -t * 0.7)), fbm(p * 1.6 + vec2<f32>(-t * 0.6, t + 3.1)));
    let field = fbm(p * 2.2 + q * warp * 2.0 + vec2<f32>(0.0, -t));

    // Many fine contour lines; each line takes its colour from where it sits.
    let density = 7.0 + 3.0 * treble();
    let rings = log(r + 0.05) * (2.0 + 1.5 * bass()) - t * 1.5;
    let v = (rings + field * 3.0) * density * 0.5;
    let line_d = abs(fract(v) - 0.5);
    let width = 0.06 + 0.05 * (1.0 - highmid());
    let line = smoothstep(width, 0.0, line_d);
    let glow = exp(-line_d * 14.0);
    let id = floor(v);
    let hue = id * 0.045 + field * 0.3 + seed() + t * 0.05;
    // Rich, saturated colours: the palette pushed and alternated with its complement.
    let c1 = palette(hue);
    let c2 = palette(hue + 0.5);
    let c = mix(c1, c2, step(0.5, fract(id * 0.5)));
    var col = c * (line * 1.6 + glow * 0.35);
    // Dark fill between lines, tinted, so the gaps read as depth not grey.
    col = col + c1 * 0.05;

    // Kick ring travelling outward.
    let kr = fract(frame.beat_phase) * 1.8;
    col = col + palette(0.2 + seed()) * kick() * exp(-abs(r - kr) * 22.0) * 2.0;

    // Hot centre.
    col = col + palette(0.1) * (0.03 + 0.2 * sub()) / (r * r * 10.0 + 0.08);

    // Endless zoom: the last frame pulled towards the centre, fading.
    let prev_uv = (in.uv - vec2<f32>(0.5)) * (0.975 - 0.015 * bass()) + vec2<f32>(0.5);
    let prev = prev_frame(prev_uv);
    col = col + prev * (0.35 + 0.35 * trails()) * 0.8;
    col = col / (1.0 + 0.15 * luma(col));

    col = col * (1.0 - 0.45 * darkness() * smoothstep(0.3, 1.6, r));
    return vec4<f32>(col, 1.0);
}
