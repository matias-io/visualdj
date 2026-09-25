// Crystal: flying through a lattice of glassy octahedra with neon edges. Every crystal
// listens to one slice of the spectrum and grows and glows with it, so the lattice shows
// where the energy is. Low-mids turn the crystals, snares flash their facets, treble lights
// the rims, the kick pulses the nearest row, and the drop throws the camera forward.

const CELL: f32 = 2.4;

struct Hit {
    d: f32,
    band: f32,
    edge: f32,
};

fn crystal_at(p: vec3<f32>, t: f32) -> Hit {
    let id = floor(p / CELL + 0.5);
    let local = p - id * CELL;
    let h = hash31(id);
    let band = fract(h * 7.31);
    let level = spectrum(band);
    let size = 0.32 + 0.45 * level + 0.1 * drop_hit();
    var q = local;
    let spin = t * (0.3 + h) + lowmid() * 1.5;
    let xz = rot2(spin) * q.xz;
    q = vec3<f32>(xz.x, q.y, xz.y);
    let xy = rot2(spin * 0.7 + h * 6.0) * q.xy;
    q = vec3<f32>(xy, q.z);
    let d = sd_octahedron(q, size);
    // Distance to the octahedron's edges, for the neon outline.
    let a = abs(q);
    let e = min(min(abs(a.x - a.y), abs(a.y - a.z)), abs(a.x - a.z));
    return Hit(d, band, e);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let uv = centred(in.uv);
    let t = music_time() * 0.5;
    let z = t * (2.0 + 2.0 * energy()) + drop_hit() * 2.0;
    let ro = vec3<f32>(CELL * 0.5 + 0.3 * sin(t * 0.4), CELL * 0.5 + 0.25 * cos(t * 0.3), z);
    var rd = normalize(vec3<f32>(uv, 1.3));
    let rxy = rot2(0.2 * sin(t * 0.17)) * rd.xy;
    rd = vec3<f32>(rxy, rd.z);

    let steps = i32(56.0 + 24.0 * quality());
    var dist = 0.0;
    var glow = vec3<f32>(0.0);
    var hit = false;
    var h = Hit(1.0, 0.0, 1.0);
    for (var i = 0; i < steps; i = i + 1) {
        let p = ro + rd * dist;
        h = crystal_at(p, t);
        let tint = palette(h.band + seed());
        glow = glow + tint * spectrum(h.band) * 0.006 / (0.02 + h.d * h.d * 30.0) * exp(-dist * 0.08);
        if (h.d < 0.001) {
            hit = true;
            break;
        }
        dist = dist + h.d * 0.8;
        if (dist > 30.0) {
            break;
        }
    }

    var col = glow * 0.6;
    if (hit) {
        let p = ro + rd * dist;
        let e = vec2<f32>(0.002, 0.0);
        let n = normalize(vec3<f32>(
            crystal_at(p + e.xyy, t).d - crystal_at(p - e.xyy, t).d,
            crystal_at(p + e.yxy, t).d - crystal_at(p - e.yxy, t).d,
            crystal_at(p + e.yyx, t).d - crystal_at(p - e.yyx, t).d,
        ));
        let tint = palette(h.band + seed());
        let level = spectrum(h.band);
        let light = normalize(vec3<f32>(0.4, 0.7, -0.6));
        let diff = max(dot(n, light), 0.0);
        let spec = pow(max(dot(reflect(rd, n), light), 0.0), 40.0);
        let fres = pow(1.0 - max(dot(-rd, n), 0.0), 3.0);
        let edge = smoothstep(0.035, 0.0, h.edge);
        let fog = exp(-dist * 0.07);
        var surf = tint * (0.04 + 0.35 * diff) * (0.3 + level);
        surf = surf + vec3<f32>(spec) * (0.5 + 2.5 * snare());
        surf = surf + palette(h.band + 0.4 + seed()) * fres * (0.4 + 1.6 * treble());
        surf = surf + tint * edge * (1.2 + 2.5 * level + 2.0 * kick() * step(dist, 6.0));
        col = col + surf * fog;
    }
    // Deep background tint.
    col = col + palette(0.7 + seed()) * 0.01 * (1.0 - darkness());
    return vec4<f32>(col, 1.0);
}
