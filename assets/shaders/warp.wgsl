// Warp: domain-warped noise in the track's colours. Slow and deep in breakdowns, faster and
// brighter as intensity rises; each beat gives a soft push of light, each onset a brief kick.

fn rotate(p: vec2<f32>, a: f32) -> vec2<f32> {
    let c = cos(a);
    let s = sin(a);
    return vec2<f32>(c * p.x - s * p.y, s * p.x + c * p.y);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let p0 = centred(in.uv);
    // Drift speed follows intensity; the bar slowly turns the whole field.
    let speed = 0.05 + 0.25 * frame.intensity;
    let t = frame.time * speed;
    let p = rotate(p0, frame.bar_phase * 0.6 + frame.time * 0.02) * 1.6;

    // Inigo Quilez's warp: q and r are themselves noise fields.
    let q = vec2<f32>(fbm(p + vec2<f32>(0.0, t)), fbm(p + vec2<f32>(5.2, 1.3) - t * 0.7));
    let r = vec2<f32>(
        fbm(p + 4.0 * q + vec2<f32>(1.7, 9.2) + 0.15 * t),
        fbm(p + 4.0 * q + vec2<f32>(8.3, 2.8) + 0.126 * t)
    );
    let f = fbm(p + 4.0 * r);

    // Low bands thicken the field, highs add sparkle.
    let low = clamp(band(1u) + band(2u) + band(3u), 0.0, 1.0);
    let high = clamp(band(18u) + band(20u) + band(22u), 0.0, 1.0);

    // Mostly background with veins of accent: keep the field dark so the accents read.
    let veins = smoothstep(0.35, 0.75, f);
    var col = mix(theme(0u), theme(2u), veins * 0.75);
    col = mix(col, theme(3u), smoothstep(0.55, 0.9, length(q)) * 0.55);
    col = mix(col, theme(4u), smoothstep(0.45, 0.8, r.x) * 0.45);

    let pulse = beat_pulse(frame.beat_phase, 3.0);
    let lift = 0.55 + 0.25 * frame.intensity + 0.2 * pulse * frame.intensity + 0.3 * frame.onset;
    col = col * lift * (0.9 + 0.25 * low);
    col = col + theme(1u) * high * 0.05 * veins;
    // A little contrast so the dark parts stay dark.
    col = pow(max(col, vec3<f32>(0.0)), vec3<f32>(1.25));

    // Approaching drop: the whole field brightens toward the accent as the countdown falls.
    if (frame.drop_countdown >= 0.0 && frame.drop_countdown <= 16.0) {
        let t2 = 1.0 - frame.drop_countdown / 16.0;
        col = mix(col, theme(2u), 0.25 * t2 * t2);
    }

    return vec4<f32>(clamp(col, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
