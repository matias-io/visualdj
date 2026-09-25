// Laser Show: beams from a truss sweep through haze over the crowd. Each lamp listens to
// its own slice of the spectrum (bass lamps in the middle, treble lamps at the ends), so
// what lights up tells you what is playing. Kick: beams punch brighter. Hats: the haze
// flickers. Bars: the sweep changes direction. Drop: every lamp fans out, white-hot.

const LAMPS: i32 = 8;

fn beam(p: vec2<f32>, o: vec2<f32>, ang: f32, width: f32) -> f32 {
    let d = vec2<f32>(cos(ang), sin(ang));
    let rel = p - o;
    let along = dot(rel, d);
    if (along < 0.0) {
        return 0.0;
    }
    let off = length(rel - d * along);
    let w = width * (1.0 + along * 1.8);
    let core = exp(-(off * off) / (w * w));
    let halo = 0.02 / (1.0 + (off / (w * 6.0)) * (off / (w * 6.0)));
    return (core + halo) / (1.0 + along * 0.35);
}

fn crowd(x: f32, t: f32) -> f32 {
    // Heads and raised hands, bobbing to the beat.
    let heads = -0.86 + 0.05 * noise2(vec2<f32>(x * 7.0, 0.0));
    let cell = floor(x * 11.0);
    let hand = step(0.72, hash21(vec2<f32>(cell, 3.0)));
    let fx = fract(x * 11.0) - 0.5;
    let bob = 0.04 * beat_pulse(frame.beat_phase, 3.0) * hash21(vec2<f32>(cell, 9.0));
    let arm_h = 0.14 + 0.08 * hash21(vec2<f32>(cell, 5.0)) + bob;
    let arm = hand * arm_h * sqrt(max(1.0 - (fx / 0.09) * (fx / 0.09), 0.0));
    let head = 0.035 * sqrt(max(1.0 - (fx / 0.4) * (fx / 0.4), 0.0));
    return heads + head + arm;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let uv = centred(in.uv);
    let t = music_time();
    let aspect = frame.resolution.x / max(frame.resolution.y, 1.0);

    // Haze: slow drifting smoke that the beams light up.
    let haze = 0.45 + 0.8 * fbm(uv * 2.2 + vec2<f32>(t * 0.08, -t * 0.05))
        + 0.25 * hat() * noise2(uv * 14.0 + t * 3.0);

    let fan = 1.0 + 3.0 * smoothstep(0.1, 0.6, drop_hit() + 0.5 * is_chorus() * frame.intensity);
    let per_lamp = i32(2.0 + floor(fan));
    let sweep_dir = select(-1.0, 1.0, fract(bar_count() * 0.5) < 0.5);
    var col = vec3<f32>(0.0);
    for (var i = 0; i < LAMPS; i = i + 1) {
        let fi = f32(i) / f32(LAMPS - 1);
        let o = vec2<f32>((fi - 0.5) * 2.0 * aspect * 0.9, 1.08);
        // Middle lamps listen to the bass, outer lamps to the treble.
        let band_pos = abs(fi - 0.5) * 2.0;
        let level = spectrum(band_pos);
        let brightness = level * level * (1.0 + 1.5 * kick()) + 0.04;
        let lamp_col = palette(fi * 0.5 + seed() + hue_shift() * 0.1);
        let hot = mix(lamp_col, vec3<f32>(1.0), 0.5 * drop_hit());
        let centre = -PI * 0.5 + 0.55 * sin(t * 0.6 * sweep_dir + fi * 3.0);
        let spread = 0.18 * fan;
        for (var j = 0; j < per_lamp; j = j + 1) {
            let fj = (f32(j) + 0.5) / f32(per_lamp) - 0.5;
            let ang = centre + fj * spread * 2.0;
            col = col + hot * beam(uv, o, ang, 0.0035) * brightness * 2.2;
        }
        // The lamp itself glows on the truss.
        col = col + lamp_col * brightness * 0.004 / (dot(uv - o, uv - o) + 0.0006);
    }
    col = col * haze;

    // Truss bar along the top.
    col = col + vec3<f32>(0.04) * smoothstep(0.012, 0.0, abs(uv.y - 1.08));

    // Stage glow from below, breathing with the sub.
    col = col + palette(0.6 + seed()) * (0.05 + 0.2 * sub()) * smoothstep(-0.2, -1.0, uv.y);

    // The crowd in silhouette, rimmed by the light behind it.
    let cy = crowd(uv.x, t);
    let body = smoothstep(cy + 0.004, cy - 0.004, uv.y);
    let rim = exp(-abs(uv.y - cy) * 90.0) * luma(col) * 0.6;
    col = mix(col, vec3<f32>(0.004, 0.003, 0.006), body) + vec3<f32>(rim);
    return vec4<f32>(col, 1.0);
}
