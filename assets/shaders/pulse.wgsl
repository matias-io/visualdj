// Pulse: the diagnostic scene. Background breathes with the beat, a bar runs along the
// bottom, the drop countdown fills a ring, and the band meter sits at the left edge.
// Every uniform the engine publishes is visible somewhere here.

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let p = centred(in.uv);
    let bg = theme(0u);
    let accent = theme(2u);
    let accent2 = theme(3u);

    // Beat breath: quick flash that decays over the beat, scaled by intensity. A vignette
    // keeps the frame from being one flat colour when nothing is playing.
    let pulse = beat_pulse(frame.beat_phase, 3.0) * (0.15 + 0.35 * frame.intensity);
    let vignette = 1.0 - smoothstep(0.1, 1.5, length(p));
    var col = bg * (0.5 + 0.9 * vignette) + accent * pulse * 0.6;

    // Bar progress along the bottom edge.
    if (in.uv.y > 0.97 && in.uv.x < frame.bar_phase) {
        col = mix(col, accent2, 0.9);
    }

    // Drop countdown ring: fills as the drop approaches (16 beats out to 0).
    if (frame.drop_countdown >= 0.0) {
        let r = length(p);
        let ang = atan2(p.y, p.x);
        let frac = clamp(1.0 - frame.drop_countdown / 16.0, 0.0, 1.0);
        let on_ring = abs(r - 0.6) < 0.02;
        let filled = (ang + 3.14159265) / 6.2831853 < frac;
        if (on_ring && filled) {
            col = mix(col, accent, 0.95);
        } else if (on_ring) {
            col = mix(col, accent, 0.15);
        }
    }

    // Band meter: 24 bars at the left edge.
    if (in.uv.x < 0.06) {
        let i = u32(clamp((1.0 - in.uv.y) * 24.0, 0.0, 23.0));
        let level = clamp(band(i) * 3.0, 0.0, 1.0);
        let within = fract((1.0 - in.uv.y) * 24.0);
        if (within > 0.15 && in.uv.x / 0.06 < level) {
            col = mix(col, theme(4u), 0.85);
        }
    }

    // Onset flash across the top edge.
    if (in.uv.y < 0.01 && frame.onset > 0.5) {
        col = theme(1u);
    }

    return vec4<f32>(col, 1.0);
}
