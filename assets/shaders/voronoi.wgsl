// Voronoi: a field of cells whose centres drift with the music. Each cell takes a colour from
// the theme and glows with one band; cell borders pulse with the beat.

const GRID_X: f32 = 16.0;
const GRID_Y: f32 = 9.0;

fn cell_centre(cell: vec2<f32>) -> vec2<f32> {
    let h = hash21(cell);
    let h2 = hash21(cell + vec2<f32>(7.7, 3.1));
    // Drift: slow wander plus a nudge from this cell's band.
    let i = u32(abs(h * 23.0)) % 24u;
    let drift = 0.25 + 0.25 * band(i);
    return cell + 0.5 + drift * vec2<f32>(
        sin(frame.time * (0.3 + h) + h2 * 6.28),
        cos(frame.time * (0.2 + h2) + h * 6.28)
    );
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let aspect = frame.resolution.x / max(frame.resolution.y, 1.0);
    let p = vec2<f32>(in.uv.x * GRID_X * aspect / 1.777, (1.0 - in.uv.y) * GRID_Y);
    let base = floor(p);

    var d1 = 10.0;
    var d2 = 10.0;
    var best = vec2<f32>(0.0);
    for (var y = -1; y <= 1; y = y + 1) {
        for (var x = -1; x <= 1; x = x + 1) {
            let cell = base + vec2<f32>(f32(x), f32(y));
            let c = cell_centre(cell);
            let d = length(p - c);
            if (d < d1) {
                d2 = d1;
                d1 = d;
                best = cell;
            } else if (d < d2) {
                d2 = d;
            }
        }
    }

    let h = hash21(best);
    let band_i = u32(abs(h * 97.0)) % 24u;
    let level = clamp(band(band_i) * 2.5, 0.0, 1.0);

    // Cell colour: pick between the accents by cell id; darken toward the background.
    let accent_pick = hash21(best + vec2<f32>(1.0, 9.0));
    var fill = theme(2u);
    if (accent_pick > 0.66) {
        fill = theme(4u);
    } else if (accent_pick > 0.33) {
        fill = theme(3u);
    }
    // Shade from a lit centre to a dark rim so no cell is a flat polygon.
    let falloff = 1.0 - smoothstep(0.0, 0.9, d1);
    let shade = (0.06 + 0.16 * h + 0.35 * level * frame.intensity) * (0.35 + 0.65 * falloff);
    var col = mix(theme(0u), fill, clamp(shade, 0.0, 0.55));

    // Borders: thin, pulsing with the beat; wider as the drop approaches.
    let pulse = beat_pulse(frame.beat_phase, 3.0);
    var border_w = 0.02 + 0.03 * pulse;
    if (frame.drop_countdown >= 0.0 && frame.drop_countdown <= 16.0) {
        border_w = border_w + 0.04 * (1.0 - frame.drop_countdown / 16.0);
    }
    let edge = 1.0 - smoothstep(0.0, border_w, d2 - d1);
    col = mix(col, theme(1u), edge * (0.2 + 0.45 * pulse));

    // Onsets light the cell centres briefly.
    let inner = 1.0 - smoothstep(0.0, 0.7, d1);
    col = col + fill * inner * (0.25 * frame.onset + 0.06 * level);

    return vec4<f32>(clamp(col, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
