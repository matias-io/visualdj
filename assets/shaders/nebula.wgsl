// Deep Space: a slow flight through a volumetric nebula. For dark and slow tracks.
// Sub and bass make the gas breathe and glow from within; mids stir it; treble and hats
// light distant stars; the kick sends a pulse of light through the cloud; the drop
// ignites the core.

fn density(p: vec3<f32>, t: f32) -> f32 {
    let warp = vec3<f32>(fbm3(p * 0.3 + vec3<f32>(0.0, 0.0, t * 0.03), 2) * 2.0);
    let d = fbm3(p * 0.45 + warp * (0.6 + 0.6 * mid()), 4);
    // Sparse clouds with empty space between them.
    return clamp((d - 0.55 + 0.06 * sub()) * 3.0, 0.0, 1.0);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let uv = centred(in.uv);
    let t = music_time() * 0.4;
    let ro = vec3<f32>(0.0, 0.0, t * 1.2);
    var rd = normalize(vec3<f32>(uv, 1.4));
    let yaw = rot2(t * 0.05 + seed() * 6.0);
    let xz = yaw * rd.xz;
    rd = vec3<f32>(xz.x, rd.y, xz.y);

    // Stars behind everything, twinkling with the highs.
    let sdir = rd * 180.0;
    let sh = hash31(sdir);
    var col = vec3<f32>(step(0.9985, sh)) * (0.6 + 1.5 * treble() * hash31(sdir + floor(t * 6.0)));
    col = col + palette(0.7 + seed()) * 0.008;

    let steps = i32(22.0 + 12.0 * quality());
    var trans = 1.0;
    var dist = 0.5;
    let step_len = 0.35;
    let pulse_r = fract(frame.beat_phase) * 8.0;
    for (var i = 0; i < steps; i = i + 1) {
        let p = ro + rd * dist;
        let d = density(p, t);
        if (d > 0.001) {
            let depth = dist / (f32(steps) * step_len);
            let tint = mix(palette(depth * 0.6 + seed()), palette(depth * 0.6 + seed() + 0.45), d);
            // Glow from within the gas: bass, a kick pulse ring and the drop.
            let inner = 0.6 + 1.6 * bass() * d + 2.5 * drop_hit() * d
                + 1.8 * kick() * exp(-abs(dist - pulse_r) * 1.5);
            // Dense cores burn hot; thin edges stay dim.
            let emit = tint * pow(d, 2.0) * inner * 1.1;
            let a = 1.0 - exp(-d * step_len * 2.2);
            col = col + emit * trans * a;
            trans = trans * (1.0 - a);
            if (trans < 0.02) {
                break;
            }
        }
        dist = dist + step_len * (1.0 + f32(i) * 0.03);
    }
    col = col * (1.0 - 0.3 * (1.0 - darkness()));
    return vec4<f32>(col, 1.0);
}
