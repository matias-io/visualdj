// Ring: 24 radial bars, one per band, around a ring that breathes with the beat. As a drop
// approaches, a second ring closes in from outside; at the drop it merges with the main ring.

const TAU: f32 = 6.28318530718;

fn soft_band(x: f32, centre: f32, half_width: f32, feather: f32) -> f32 {
    let d = abs(x - centre);
    return 1.0 - smoothstep(half_width - feather, half_width + feather, d);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let p = centred(in.uv);
    let r = length(p);
    let ang = atan2(p.y, p.x);

    let bg = theme(0u);
    let bar_col = mix(theme(2u), theme(3u), frame.intensity);
    let glow_col = theme(4u);

    // Background: dark vignette so the frame is never flat, plus a faint noise grain.
    let vignette = 1.0 - smoothstep(0.2, 1.4, r);
    var col = bg * (0.6 + 0.6 * vignette) + 0.02 * noise2(p * 3.0 + vec2<f32>(frame.time * 0.05, 0.0));

    // Main ring radius breathes with the beat, more when the music is intense.
    let pulse = beat_pulse(frame.beat_phase, 4.0);
    let ring_r = 0.52 + 0.05 * pulse * (0.3 + frame.intensity);
    let ring = soft_band(r, ring_r, 0.012, 0.006);
    col = mix(col, bar_col, ring * 0.9);

    // Radial bars: sector per band, rotating slowly with the bar.
    let rot = ang + frame.bar_phase * TAU * 0.25 + frame.time * 0.05;
    let sector = (rot / TAU + 1.0) * 24.0;
    let i = u32(floor(fract(sector / 24.0) * 24.0)) % 24u;
    let within = fract(sector);
    let level = clamp(band(i) * 2.2 + 0.08, 0.0, 1.0);
    let bar_len = 0.06 + 0.32 * level;
    let in_bar = step(ring_r + 0.02, r) * (1.0 - step(ring_r + 0.02 + bar_len, r));
    let bar_shape = in_bar * soft_band(within, 0.5, 0.32, 0.08);
    let bar_shade = mix(bar_col, glow_col, smoothstep(ring_r, ring_r + bar_len + 0.02, r) * 0.6);
    col = mix(col, bar_shade, bar_shape * (0.55 + 0.45 * level));

    // Inner glow that flashes on onsets and pulses with the beat.
    let inner = 1.0 - smoothstep(0.0, ring_r - 0.02, r);
    let flash = 0.08 * frame.intensity + 0.25 * frame.onset + 0.12 * pulse;
    col = col + glow_col * inner * flash;

    // Drop countdown: an outer ring that closes in over 16 beats and lands on the main ring.
    if (frame.drop_countdown >= 0.0 && frame.drop_countdown <= 16.0) {
        let t = 1.0 - frame.drop_countdown / 16.0;
        let cd_r = ring_r + 0.42 * (1.0 - t * t);
        let cd = soft_band(r, cd_r, 0.006 + 0.01 * t, 0.006);
        col = mix(col, theme(1u), cd * (0.35 + 0.65 * t));
    }

    return vec4<f32>(col, 1.0);
}
