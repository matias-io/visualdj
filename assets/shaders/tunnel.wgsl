// Neon Tunnel: flying through glowing diamond gates.
// Bass: speed and gate glow. Low-mids: the twist. Mids/high-mids: side struts light up
// along the spectrum. Treble and hats: sparks. Kick: gates flare. Drop: the tunnel opens,
// glow doubles and the colours swing.

const GATE_SPACING: f32 = 2.2;

fn gate_radius(id: f32) -> f32 {
    return 1.25 + 0.18 * sin(id * 1.7 + seed() * 6.0);
}

// Distance to one gate: a diamond ring plus a square frame turned 45 degrees inside it.
fn gate(p: vec3<f32>, id: f32, twist: f32) -> f32 {
    var q = p;
    let r = gate_radius(id) * (1.0 + 0.25 * drop_hit());
    let spin = rot2(twist + id * 0.35);
    let xy = spin * q.xy;
    q = vec3<f32>(xy, q.z);
    let diamond = abs(abs(q.x) + abs(q.y) - r) * 0.7071;
    let ring = length(vec2<f32>(diamond, q.z)) - 0.018;
    let sq = sd_box_frame(vec3<f32>(rot2(0.7854) * q.xy, q.z), vec3<f32>(r * 0.62, r * 0.62, 0.04), 0.012);
    return min(ring, sq);
}

// Struts along the walls, one per spectrum slice, lit by that slice's level.
fn struts(p: vec3<f32>) -> vec2<f32> {
    let a = atan2(p.y, p.x);
    let n = 16.0;
    let sector = floor((a / TAU + 0.5) * n);
    let ang = (sector + 0.5) / n * TAU - PI;
    let dir = vec2<f32>(cos(ang), sin(ang));
    let radial = dot(p.xy, dir);
    let side = length(p.xy - dir * radial);
    let d = length(vec2<f32>(side, radial - 2.1)) - 0.012;
    return vec2<f32>(d, sector / n);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let uv = centred(in.uv);
    let t = music_time();
    let fov = 1.1 - 0.15 * bass() - 0.1 * drop_hit();
    let z0 = t * (2.6 + 1.4 * energy()) + frame.playhead * 0.0;
    let ro = vec3<f32>(0.12 * sin(t * 0.37), 0.1 * cos(t * 0.29), z0);
    var rd = normalize(vec3<f32>(uv * fov, 1.6));
    let roll = 0.15 * sin(t * 0.21) + 0.4 * phrase_hit() * sin(bar_count());
    rd = vec3<f32>(rot2(roll) * rd.xy, rd.z);

    let twist = t * 0.15 + 0.8 * lowmid();
    let steps = i32(40.0 + 20.0 * quality());
    var dist = 0.05;
    var col = vec3<f32>(0.0);
    for (var i = 0; i < steps; i = i + 1) {
        let p = ro + rd * dist;
        let id = floor(p.z / GATE_SPACING + 0.5);
        let local = vec3<f32>(p.xy, p.z - id * GATE_SPACING);
        let dg = gate(local, id, twist);
        let st = struts(local);
        let fade = exp(-dist * 0.06);

        // Gates: palette colour per gate, flaring on the kick and at the drop.
        let hue = id * 0.07 + seed() + hue_shift() * 0.2;
        let gate_col = palette(hue);
        let flare = 1.0 + 1.2 * kick() * step(0.5, fract(id * 0.5 + 0.25)) + 0.8 * drop_hit();
        col = col + gate_col * flare * fade * (0.0018 / (0.00035 + dg * dg));

        // Struts: brightness from the spectrum slice they stand for.
        let level = spectrum(st.y);
        let strut_col = palette(st.y + 0.33 + seed());
        col = col + strut_col * level * level * fade * (0.0009 / (0.0004 + st.x * st.x));

        dist = dist + max(min(dg, st.x) * 0.7, 0.02);
        if (dist > 38.0) {
            break;
        }
    }
    col = col / f32(steps) * 7.0;

    // The light at the end of the tunnel breathes with the sub.
    let r = length(uv);
    col = col + theme(3u) * (0.02 + 0.06 * sub()) / (r * r + 0.015);

    // Sparks: thin streaks that fly past on the hats and treble.
    let lp = vec2<f32>(atan2(uv.y, uv.x) * 18.0 / PI, log(r + 0.001) * 5.0 - t * 6.0);
    let cell = floor(lp);
    let f = fract(lp);
    let h = hash21(cell);
    let streak = exp(-abs(f.x - 0.5) * 60.0) * smoothstep(0.0, 0.3, f.y) * smoothstep(1.0, 0.5, f.y);
    let spark = step(0.93 - 0.06 * treble(), h) * streak * (0.3 + 1.2 * hat());
    col = col + vec3<f32>(spark) * palette(h) * smoothstep(0.25, 1.0, r);

    // Darker between gates when the track is dark.
    col = col * (1.0 - 0.35 * darkness() * (1.0 - intensity_or_play()));
    return vec4<f32>(col, 1.0);
}

fn intensity_or_play() -> f32 {
    return max(frame.intensity, 0.3);
}
