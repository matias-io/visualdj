// Silk: a sheet of glowing lines seen in perspective, undulating like cloth in wind.
// The sheet's shape across its width is the spectrum (bass in the middle, treble at the
// edges); a kick sends a ripple out from the centre; snares shimmer along the lines; the
// drop lifts the whole sheet and brightens it.

fn surface(x: f32, z: f32, t: f32) -> f32 {
    let u = clamp(abs(x) * 0.28, 0.0, 1.0);
    let s = spectrum(u);
    var h = 0.28 * sin(x * 1.1 + t * 0.8 + z * 2.2 + seed() * 6.0);
    h = h + 0.18 * sin(x * 0.55 - t * 0.5 + z * 3.3);
    h = h + (0.15 + 0.55 * s) * sin(x * 2.6 + z * 4.5 - t * 1.7);
    // Kick ripple travelling outward from the middle of the sheet.
    let r = length(vec2<f32>(x, (z - 1.6) * 2.5));
    let ripple = sin(r * 5.0 - fract(frame.beat_phase) * 12.0) * exp(-r * 0.45);
    h = h + 0.35 * kick() * ripple;
    h = h * (0.7 + 0.5 * loudness() + 0.4 * drop_hit());
    return h;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let uv = centred(in.uv);
    let t = music_time() * 0.9;
    let focal = 1.4;
    let cam_y = 0.9 - 0.25 * drop_hit();
    let lines = i32(28.0 + 14.0 * quality());
    var col = vec3<f32>(0.0);
    // Far lines only add their soft halo; it is tinted once, after the loop.
    var far_glow = 0.0;
    var far_fi = 0.0;
    for (var i = 0; i < lines; i = i + 1) {
        let fi = f32(i) / f32(lines);
        let z = 0.9 + fi * 3.2;
        let x = uv.x * z / focal;
        let h = surface(x, z, t);
        let y = (h - cam_y) * focal / z + 0.35;
        let d = abs(uv.y - y);
        let depth_fade = 1.0 - fi * 0.75;
        let halo = 0.0009 / (d * d + 0.0009) * 0.06;
        if (d > 0.06) {
            far_glow = far_glow + halo * depth_fade;
            far_fi = far_fi + halo * depth_fade * fi;
            continue;
        }
        let width = 0.0035 / z + 0.0008;
        let core = exp(-(d * d) / (width * width));
        // Colour runs across the sheet and back to front; highs sparkle along the lines.
        let tint = palette(fi * 0.6 + x * 0.05 + seed() + t * 0.02);
        let sparkle = 1.0 + 2.0 * snare() * noise2(vec2<f32>(x * 12.0, fi * 30.0 + t * 4.0))
            + 1.5 * treble() * step(0.8, noise2(vec2<f32>(x * 30.0 - t * 6.0, fi * 50.0)));
        col = col + tint * (core * 1.4 + halo) * sparkle * depth_fade;
    }
    if (far_glow > 0.0) {
        let x = uv.x * 2.0 / focal;
        col = col + palette(far_fi / far_glow * 0.6 + x * 0.05 + seed() + t * 0.02) * far_glow;
    }
    // A haze of colour behind the sheet, rising with the mids.
    // Vocals add a soft glow over the sheet, like a spotlight on the singer.
    let haze = palette(0.5 + seed()) * (0.04 + 0.1 * mid() + 0.12 * vocal()) * smoothstep(-0.6, 0.8, uv.y);
    col = col + haze * (1.0 - darkness() * 0.6);
    return vec4<f32>(col, 1.0);
}
