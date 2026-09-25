// Outrun: a striped sun over a neon grid. The mountains on either side of the road are
// the last two seconds of the spectrum, bass nearest the road and treble at the edges,
// flowing towards the camera. Kick: the sun swells. Snare: the grid flashes. Hats: stars.
// Drop: the sun flares and the grid runs faster.

const HORIZON: f32 = 0.12;
const DEPTH: f32 = 40.0;
const ROAD: f32 = 1.4;

// Terrain height at ground point (x, z) (z grows away from the camera).
fn terrain(x: f32, z: f32) -> f32 {
    let side = abs(x);
    let off_road = smoothstep(ROAD, ROAD + 2.5, side);
    // Spectrum position grows with distance from the road; age with depth, so new audio
    // appears at the horizon and flows towards the viewer.
    let u = clamp((side - ROAD) / 9.0, 0.0, 1.0);
    let age = clamp(1.0 - z / DEPTH, 0.0, 1.0);
    // Smoothed over neighbouring bands and moments, so ridges read as mountains.
    let s = (history(u, age) * 2.0 + history(u + 0.05, age) + history(u - 0.05, age)
        + history(u, age + 0.03) + history(u, age - 0.03)) / 6.0;
    let ridge = noise2(vec2<f32>(x * 0.35, z * 0.25)) * 0.6;
    let peaks = pow(s, 1.3) * 5.5 + ridge * (1.0 + 2.0 * u);
    return off_road * peaks * (0.5 + 0.8 * u);
}

fn grid_line(v: f32, width: f32) -> f32 {
    let f = abs(fract(v) - 0.5);
    return smoothstep(0.5 - width, 0.5, f);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let uv = centred(in.uv);
    let t = music_time();
    let speed = 2.0 + 1.5 * energy() + 2.0 * drop_hit();
    let scroll = t * speed * 2.0;
    let accent_a = palette(0.0 + seed());
    let accent_b = palette(0.4 + seed());

    // Sky: deep at the top, warm at the horizon.
    let sky_t = clamp((uv.y - HORIZON) / 1.1, 0.0, 1.0);
    var col = mix(accent_b * 0.35, vec3<f32>(0.02, 0.0, 0.05), pow(sky_t, 0.6));

    // Stars, twinkling with the hats.
    let sc = floor(uv * 90.0);
    let sh = hash21(sc);
    let twinkle = step(0.992, sh) * (0.4 + 1.2 * hat() * hash21(sc + floor(t * 4.0)));
    col = col + vec3<f32>(twinkle) * smoothstep(HORIZON + 0.1, HORIZON + 0.5, uv.y);

    // Sun: a striped disc sitting on the horizon, swelling on the kick.
    let sun_c = vec2<f32>(0.0, HORIZON + 0.28);
    let sun_r = 0.46 + 0.03 * kick() + 0.06 * drop_hit();
    let d = length(uv - sun_c);
    let gy = (uv.y - sun_c.y) / sun_r;
    let stripes = step(0.5, fract(gy * 7.0 - t * 0.4)) + step(0.1, gy);
    let band_gap = clamp(stripes, 0.0, 1.0) * smoothstep(-0.9, 0.2, gy) + step(0.0, gy);
    let sun_mask = smoothstep(sun_r, sun_r - 0.005, d) * clamp(band_gap, 0.0, 1.0);
    // Hot yellow at the top of the disc, the track's accent at the bottom.
    let sun_col = mix(accent_a * 1.1, vec3<f32>(1.25, 0.95, 0.25), clamp(gy * 0.6 + 0.5, 0.0, 1.0));
    col = mix(col, sun_col * (1.0 + 0.6 * drop_hit() + 0.3 * kick()), sun_mask * step(HORIZON, uv.y));
    col = col + accent_a * 0.25 * exp(-max(d - sun_r, 0.0) * 5.0) * step(HORIZON, uv.y);

    // Ground: march the terrain along the ray.
    if (uv.y < HORIZON + 0.35) {
        let cam_h = 1.3;
        let ro = vec3<f32>(0.0, cam_h, 0.0);
        let rd = normalize(vec3<f32>(uv.x, uv.y - HORIZON, 1.0));
        var z = 0.3;
        var hit = false;
        var p = vec3<f32>(0.0);
        let steps = i32(40.0 + 24.0 * quality());
        var last_gap = 1.0;
        for (var i = 0; i < steps; i = i + 1) {
            p = ro + rd * z;
            let h = terrain(p.x, p.z);
            let gap = p.y - h;
            if (gap < 0.0) {
                // Step back half way to soften the silhouette.
                z = z - 0.5 * min(last_gap, 0.3);
                p = ro + rd * z;
                hit = true;
                break;
            }
            last_gap = gap;
            z = z + max(gap * 0.45, 0.05 + z * 0.012);
            if (p.z > DEPTH) {
                break;
            }
        }
        if (hit) {
            let h = terrain(p.x, p.z);
            let gx = grid_line(p.x * 0.8, 0.04 + 0.02 * p.z / DEPTH);
            let gz = grid_line((p.z + scroll) * 0.8, 0.04 + 0.02 * p.z / DEPTH);
            let grid = max(gx, gz);
            let fog = exp(-p.z * 0.045);
            let height_col = mix(accent_b, accent_a, clamp(h / 2.5, 0.0, 1.0));
            let glow = 1.6 + 2.5 * snare() * fog + 1.2 * drop_hit();
            // Black ground and mountains, drawn only by their glowing grid.
            let mountain = smoothstep(0.2, 1.0, h);
            var ground = vec3<f32>(0.005, 0.0, 0.01);
            let line_col = mix(height_col, accent_a * 1.2, mountain);
            ground = ground + line_col * grid * glow * fog;
            col = mix(col, ground, 1.0 - smoothstep(0.0, 1.0, p.z / DEPTH) * 0.8);
        } else if (uv.y < HORIZON) {
            col = col + accent_b * 0.15;
        }
    }

    // Heat haze at the horizon.
    col = col + accent_a * 0.35 * exp(-abs(uv.y - HORIZON) * 22.0);
    return vec4<f32>(col, 1.0);
}
