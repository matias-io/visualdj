// Zero Gravity: floating through a field of tumbling debris in deep space. Nothing falls;
// everything drifts. The bass breathes the whole field outward, mids turn the pieces,
// hats catch glints of dust, vocals light the rims, the kick lights the nearest edges,
// and the build-up draws everything towards the middle until the drop throws it apart.

const ZG_CELL: f32 = 4.2;

struct ZgHit {
    d: f32,
    h: f32,
};

fn sd_round_box(p: vec3<f32>, b: vec3<f32>, r: f32) -> f32 {
    let q = abs(p) - b;
    return length(max(q, vec3<f32>(0.0))) + min(max(q.x, max(q.y, q.z)), 0.0) - r;
}

fn debris(p_in: vec3<f32>, t: f32) -> ZgHit {
    // Breathe: the bass pushes the field away from the flight axis, the build-up pulls it
    // in, the drop throws it out.
    let radial = vec2<f32>(p_in.x, p_in.y);
    let push = 1.0 + 0.18 * bass() - 0.22 * tension() + 0.45 * drop_hit();
    let p = vec3<f32>(radial / push, p_in.z);
    let id = floor(p / ZG_CELL + 0.5);
    let h = hash31(id);
    // Each piece drifts in its own slow orbit around its cell centre.
    let drift = vec3<f32>(
        sin(t * (0.3 + h) + h * 11.0),
        cos(t * (0.25 + 0.5 * h) + h * 7.0),
        sin(t * 0.2 + h * 3.0),
    ) * 0.55;
    var q = p - id * ZG_CELL - drift;
    // Tumble.
    let spin = t * (0.2 + 0.6 * h) + lowmid() * 1.2 + h * 20.0;
    let xz = rot2(spin) * q.xz;
    q = vec3<f32>(xz.x, q.y, xz.y);
    let xy = rot2(spin * 0.7) * q.xy;
    q = vec3<f32>(xy, q.z);
    let size = 0.3 + 0.38 * fract(h * 13.7);
    var d: f32;
    if (fract(h * 5.3) < 0.55) {
        d = sd_round_box(q, vec3<f32>(size, size * 0.7, size * 1.1), 0.04);
    } else {
        d = sd_octahedron(q, size * 1.3);
    }
    return ZgHit(d * min(push, 1.0), h);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let uv = centred(in.uv);
    let t = music_time() * 0.35;
    // A slow float: forward drift, a gentle roll, no bumps.
    let ro = vec3<f32>(0.6 * sin(t * 0.21), 0.5 * cos(t * 0.17), t * 1.6);
    var rd = normalize(vec3<f32>(uv, 1.5));
    let roll = rot2(0.25 * sin(t * 0.13) + t * 0.03);
    let rxy = roll * rd.xy;
    rd = vec3<f32>(rxy, rd.z);

    // Deep space: a faint nebula wash and stars.
    let neb = fbm(rd.xy * 1.6 + vec2<f32>(t * 0.02, seed() * 9.0));
    var col = mix(palette(0.65 + seed()), palette(0.9 + seed()), neb) * (0.012 + 0.02 * neb * neb);
    let sdir = rd * 220.0;
    col = col + vec3<f32>(step(0.9982, hash31(sdir))) * (0.4 + 1.2 * hat() * hash31(sdir + floor(t * 8.0)));

    let steps = i32(40.0 + 16.0 * quality());
    var dist = 0.3;
    var hit = false;
    var h = ZgHit(1.0, 0.0);
    for (var i = 0; i < steps; i = i + 1) {
        h = debris(ro + rd * dist, t);
        if (h.d < 0.002) {
            hit = true;
            break;
        }
        dist = dist + h.d * 0.9;
        if (dist > 28.0) {
            break;
        }
    }

    if (hit) {
        let p = ro + rd * dist;
        let e = vec2<f32>(0.003, 0.0);
        let n = normalize(vec3<f32>(
            debris(p + e.xyy, t).d - debris(p - e.xyy, t).d,
            debris(p + e.yxy, t).d - debris(p - e.yxy, t).d,
            debris(p + e.yyx, t).d - debris(p - e.yyx, t).d,
        ));
        let tint = palette(h.h * 0.8 + seed());
        let key = normalize(vec3<f32>(-0.5, 0.6, -0.4));
        let diff = max(dot(n, key), 0.0);
        let spec = pow(max(dot(reflect(rd, n), key), 0.0), 32.0);
        let fres = pow(1.0 - max(dot(-rd, n), 0.0), 3.0);
        let fog = exp(-dist * 0.075);
        var surf = tint * (0.02 + 0.3 * diff);
        surf = surf + vec3<f32>(spec) * (0.4 + 1.5 * snare());
        // Rim light in the second palette colour, brighter with vocals.
        surf = surf + palette(h.h + 0.45 + seed()) * fres * (0.35 + 1.4 * vocal() + 0.6 * treble());
        // The kick lights the nearest pieces from inside.
        surf = surf + tint * kick() * 1.2 * step(dist, 7.0) * fres;
        col = col * (1.0 - fog) + surf * fog;
    }

    // Dust motes drifting past, glinting with the hats.
    let dust_uv = uv * 9.0 + vec2<f32>(t * 0.4, -t * 0.25);
    let cell = floor(dust_uv);
    let f = fract(dust_uv) - hash22(cell);
    let mote = exp(-dot(f, f) * 900.0) * step(0.8, hash21(cell));
    col = col + palette(0.2 + seed()) * mote * (0.15 + 1.2 * hat());
    return vec4<f32>(col, 1.0);
}
