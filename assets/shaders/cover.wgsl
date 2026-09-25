// Cover Art: the track's artwork as the centrepiece. A slowly turning square of the cover
// with mirrored echoes radiating outward, pulled by the spectrum; snares split the colour
// channels; the kick pumps the scale; the drop shatters it into flying shards.

fn cover_uv(p: vec2<f32>, scale: f32, spin: f32) -> vec2<f32> {
    let q = rot2(spin) * p / scale;
    return q * 0.5 + vec2<f32>(0.5);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let uv = centred(in.uv);
    let t = music_time() * 0.3;
    let r = length(uv);
    let ang = atan2(uv.y, uv.x);

    // Shatter: Voronoi shards pushed outward at the drop.
    var p = uv;
    let cell_p = uv * 5.0;
    let cell = floor(cell_p);
    var best = 10.0;
    var shard = vec2<f32>(0.0);
    for (var y = -1; y <= 1; y = y + 1) {
        for (var x = -1; x <= 1; x = x + 1) {
            let c = cell + vec2<f32>(f32(x), f32(y));
            let o = c + hash22(c);
            let d = length(cell_p - o);
            if (d < best) {
                best = d;
                shard = o;
            }
        }
    }
    let push = drop_hit() * 0.6;
    let shard_dir = normalize(shard / 5.0 + vec2<f32>(1e-4));
    p = p - shard_dir * push * (0.3 + hash21(shard));
    p = rot2(push * (hash21(shard + 7.0) - 0.5) * 2.0) * p;

    // Spectrum pulls the picture into a radial wave.
    let wave = spectrum(fract(ang / TAU + 0.5)) * 0.05 * (1.0 + r);
    p = p * (1.0 - wave);

    let scale = 0.55 + 0.04 * kick() + 0.05 * sin(t * 0.7);
    let spin = t * 0.2 + 0.1 * sin(t * 0.5);
    let q = rot2(spin) * p / scale;
    // Mirrored echoes: fold the plane into repeating reflections of the cover.
    let tile = abs(fract(q * 0.5 + 0.5) * 2.0 - 1.0);
    let echo = max(abs(q.x), abs(q.y));
    let split = 0.004 + 0.02 * snare();
    let dir = normalize(q + vec2<f32>(1e-4)) * split;
    var art = vec3<f32>(
        artwork(tile + dir).r,
        artwork(tile).g,
        artwork(tile - dir).b,
    );
    // The central copy bright, echoes fading and tinted.
    let centre = step(echo, 1.0);
    let fade = exp(-(echo - 1.0) * 0.9);
    let tint = mix(vec3<f32>(1.0), palette(floor(echo) * 0.2 + seed()), 0.5);
    var col = art * mix(tint * fade * 0.45, vec3<f32>(1.0), centre);

    // Glowing frame around the central copy, lit by the mids.
    let edge = abs(echo - 1.0);
    col = col + palette(0.2 + seed()) * exp(-edge * 90.0) * (0.6 + 2.0 * mid());

    // Treble shimmer across the whole frame.
    col = col * (1.0 + 0.25 * treble() * noise2(uv * 30.0 + t * 8.0));
    col = col * (1.0 - 0.3 * darkness() * smoothstep(0.4, 1.8, r));
    return vec4<f32>(col, 1.0);
}
