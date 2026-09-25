// Deep Space: a slow flight through a volumetric nebula. For dark and slow tracks.
// Sub and bass make the gas breathe and glow from within; mids stir it; treble and hats
// light distant stars; the kick sends a pulse of light through the cloud; the drop
// ignites the core.

// Value noise with a sine hash: the eight corners share one lattice index, so each costs a
// single sine. Plenty for soft gas, and far cheaper than the integer hash in common.wgsl.
fn gas_noise(p: vec3<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let n = dot(i, vec3<f32>(1.0, 57.0, 113.0));
    let h = fract(sin(vec4<f32>(n, n + 1.0, n + 57.0, n + 58.0) * 0.0137) * 43758.5453);
    let k = fract(sin(vec4<f32>(n + 113.0, n + 114.0, n + 170.0, n + 171.0) * 0.0137) * 43758.5453);
    let lo = mix(mix(h.x, h.y, u.x), mix(h.z, h.w, u.x), u.y);
    let hi = mix(mix(k.x, k.y, u.x), mix(k.z, k.w, u.x), u.y);
    return mix(lo, hi, u.z);
}

fn density(p: vec3<f32>, t: f32) -> f32 {
    // A smooth sine warp swirls the gas; cheaper than a noise lookup and just as soft.
    let warp = sin(p.yzx * 0.37 + vec3<f32>(t * 0.05, 1.7, 4.1)) * 0.9;
    let q = p * 0.45 + warp * (0.6 + 0.6 * mid());
    let cut = 0.55 - 0.06 * sub();
    // Octave by octave, stop as soon as the rest cannot reach the cut: octaves after the
    // first add at most 0.44, after the second at most 0.19.
    let o1 = 0.5 * gas_noise(q);
    if (o1 < cut - 0.44) {
        return 0.0;
    }
    let coarse = o1 + 0.25 * gas_noise(q * 2.02 + vec3<f32>(17.1, 9.2, 4.7));
    if (coarse < cut - 0.19) {
        return 0.0;
    }
    let q2 = q * 4.0804 + vec3<f32>(51.6, 27.8, 14.2);
    let d = coarse + 0.125 * gas_noise(q2) + 0.0625 * gas_noise(q2 * 2.02 + vec3<f32>(17.1, 9.2, 4.7));
    // Sparse clouds with empty space between them.
    return clamp((d - cut) * 3.0, 0.0, 1.0);
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

    let steps = i32(12.0 + 6.0 * quality());
    let step_len = 0.62;
    var trans = 1.0;
    // A per-pixel offset hides the banding that fewer, longer steps would leave.
    // It changes every frame, so it reads as grain rather than a pattern fixed to the glass.
    let jitter = fract(hash21(in.uv * frame.resolution) + frame.time * 61.8034);
    var dist = 0.5 + step_len * jitter;
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
        // Empty space is crossed in longer strides.
        dist = dist + step_len * (1.0 + f32(i) * 0.03) * select(1.0, 1.6, d <= 0.001);
    }
    col = col * (1.0 - 0.3 * (1.0 - darkness()));
    return vec4<f32>(col, 1.0);
}
