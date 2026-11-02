// Experimental production bridge: one shared setup per edge, compact active triangle records,
// independent cheap cap-time coverage for parallel motion, GPU indirect counts.
// PARALLEL_SUPPORT_MODE
struct FrameParams {
    rotation: array<vec4<f32>, 3>,
    // viewport.xy is full physical extent; zw maps model pixels to physical pixels.
    viewport: vec4<f32>, camera: vec4<f32>, pose: vec4<f32>,
    style: vec4<f32>, tint: vec4<f32>, history: vec4<u32>,
    temporal: vec4<f32>,
}

struct CompactTriangle {
    // Clip-space a.xy, b.xy, then c.xy and two alpha values.
    points: vec4<f32>,
    tail: vec4<f32>,
    // Mixed fields occupy one 16-byte lane without float-packing the tag.
    alpha: f32,
    flags: u32,
    first_normal: vec2<f32>,
    // First AA offset, second AA normal.xy/offset.
    planes: vec4<f32>,
}
struct ParallelInterval {
    first: vec4<f32>, last: vec4<f32>, depths: vec4<f32>,
    // Normalized along direction, temporal interval, projected-edge detail weight.
    setup: vec4<f32>,
}
struct BodyData { attachment: vec4<f32>, along: vec4<f32>, }
struct Counters { words: array<atomic<u32>, 28>, }

@group(0) @binding(0) var<uniform> params: FrameParams;
@group(0) @binding(1) var<storage, read> edges: array<vec2<u32>>;
@group(0) @binding(2) var<storage, read> history: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read_write> output_triangles: array<CompactTriangle>;
@group(0) @binding(4) var<storage, read_write> output_parallel: array<ParallelInterval>;
@group(0) @binding(5) var<storage, read_write> counters: Counters;
@group(0) @binding(6) var<storage, read_write> output_bodies: array<BodyData>;
@group(0) @binding(7) var<storage, read_write> output_masks: array<vec4<u32>>;
@group(0) @binding(8) var<storage, read> edge_styles: array<vec4<f32>>;

// Plain function data, not the rendering interface. Shared geometry helpers
// produce the same positions and coefficients as the direct vertex path.
struct RibbonVertex {
    position: vec4<f32>, first: vec4<f32>, last: vec4<f32>, depths: vec4<f32>,
    interval: u32, fast_mode: u32, ink: vec4<f32>,
    support_a: vec4<f32>, support_b: vec4<f32>,
}
fn cross2(a: vec2<f32>, b: vec2<f32>) -> f32 { return a.x * b.y - a.y * b.x; }
fn support_edge(a: vec2<f32>, b: vec2<f32>, enabled: bool) -> vec4<f32> {
    let edge = b - a;
    let normal = vec2(-edge.y, edge.x) * inverseSqrt(max(dot(edge, edge), 1e-12));
    return vec4(normal, -dot(normal, a), select(0.0, 1.0, enabled));
}

// SHARED_FAST_GEOMETRY

fn wholly_outside(low: vec2<f32>, high: vec2<f32>, padding: f32) -> bool {
    return high.x < -padding || high.y < -padding
        || low.x > params.viewport.x + padding || low.y > params.viewport.y + padding;
}

@compute @workgroup_size(64)
fn prepare_edges(@builtin(global_invocation_id) id: vec3<u32>) {
    let edge_count = arrayLength(&edges);
    if id.x == 0u {
        atomicStore(&counters.words[0], 3u);
        atomicStore(&counters.words[4], 6u);
        atomicStore(&counters.words[8], edge_count);
        atomicStore(&counters.words[16], 3u);
        atomicStore(&counters.words[20], 3u);
        atomicStore(&counters.words[24], 3u);
    }
    if id.x >= edge_count { return; }
    // Every live edge starts empty, including all following early-return paths.
    output_masks[id.x] = vec4(0u);
    if edge_styles[id.x].a <= 0.0 {
        atomicAdd(&counters.words[9], 1u);
        return;
    }
    let edge = prepare_fast_edge(id.x);
    let low = min(min(min(edge.a0.xy, edge.b0.xy), min(edge.am.xy, edge.bm.xy)), min(edge.a1.xy, edge.b1.xy));
    let high = max(max(max(edge.a0.xy, edge.b0.xy), max(edge.am.xy, edge.bm.xy)), max(edge.a1.xy, edge.b1.xy));
    if wholly_outside(low, high, edge.half + 0.75) {
        atomicAdd(&counters.words[9], 1u);
        return;
    }
    if edge.parallel != 0u {
        var membership = vec4(0u);
        var count = 0u;
        for (var interval = 0u; interval < 2u; interval += 1u) {
            let a0 = select(edge.a0, edge.am, interval == 1u);
            let b0 = select(edge.b0, edge.bm, interval == 1u);
            let a1 = select(edge.am, edge.a1, interval == 1u);
            let b1 = select(edge.bm, edge.b1, interval == 1u);
            if min(min(a0.w, b0.w), min(a1.w, b1.w)) < 0.5 { continue; }
            if wholly_outside(min(min(a0.xy, b0.xy), min(a1.xy, b1.xy)),
                max(max(a0.xy, b0.xy), max(a1.xy, b1.xy)), edge.half + 0.75) { continue; }
            var direction = (b0.xy - a0.xy) + (b1.xy - a1.xy);
            if dot(direction, direction) < 1e-10 { direction = vec2(1.0, 0.0); }
            output_parallel[id.x * 2u + interval] = ParallelInterval(vec4(a0.xy, b0.xy), vec4(a1.xy, b1.xy),
                vec4(a0.z, b0.z, a1.z, b1.z), vec4(normalize(direction), f32(interval), 0.0));
            membership.w |= 1u << interval;
            count += 1u;
        }
        if count > 0u {
            atomicAdd(&counters.words[5], count);
            atomicAdd(&counters.words[13], 1u);
        } else { atomicAdd(&counters.words[9], 1u); }
        output_masks[id.x] = membership;
        return;
    }
    if edge.visible == 0u {
        atomicAdd(&counters.words[9], 1u);
        return;
    }
    var body_written = false;
    var membership = vec4(0u);
    var count = 0u;
    var categories = vec4(0u);
    let clip_padding = vec2(2.0) / params.viewport.xy;
    for (var triangle = 0u; triangle < 6u; triangle += 1u) {
        let a = fast_corner(triangle * 3u, edge);
        let b = fast_corner(triangle * 3u + 1u, edge);
        let c = fast_corner(triangle * 3u + 2u, edge);
        let low_clip = min(min(a.position.xy, b.position.xy), c.position.xy);
        let high_clip = max(max(a.position.xy, b.position.xy), c.position.xy);
        let area = abs(cross2(b.position.xy - a.position.xy, c.position.xy - a.position.xy));
        if area < 1e-15 || max(max(a.ink.a, b.ink.a), c.ink.a) == 0.0
            || high_clip.x < -1.0 - clip_padding.x || high_clip.y < -1.0 - clip_padding.y
            || low_clip.x > 1.0 + clip_padding.x || low_clip.y > 1.0 + clip_padding.y {
            categories.w += 1u;
            continue;
        }
        // Low bits: mode0/2/4, first/second support enabled, body side.
        // Higher bits identify shared per-edge body attachment data.
        let flags = a.fast_mode | select(0u, 8u, a.support_a.w > 0.5)
            | select(0u, 16u, a.support_b.w > 0.5) | (a.interval << 5u) | (id.x << 6u);
        let record_index = id.x * 6u + triangle;
        output_triangles[record_index] = CompactTriangle(vec4(a.position.xy, b.position.xy),
            vec4(c.position.xy, a.ink.a, b.ink.a),
            c.ink.a, flags, a.support_a.xy,
            vec4(a.support_a.z, a.support_b.xyz));
        var category = 0u;
        if a.fast_mode == 2u { category = 1u; }
        if a.fast_mode == 4u {
            category = 2u;
            if !body_written {
                let segment = edge.bm.xy - edge.am.xy;
                let along = segment / max(dot(segment, segment), 1e-10);
                output_bodies[id.x] = BodyData(edge.attachment, vec4(along, -dot(along, edge.am.xy), 0.0));
                body_written = true;
            }
        }
        membership[category] |= 1u << triangle;
        count += 1u;
        if triangle == 2u || triangle == 3u { categories.x += 1u; }
        else if a.fast_mode == 2u { categories.z += 1u; }
        else { categories.y += 1u; }
    }
    if count > 0u {
        atomicAdd(&counters.words[1], count);
        for (var category = 0u; category < 3u; category += 1u) {
            let size = countOneBits(membership[category]);
            if size == 0u { continue; }
            atomicAdd(&counters.words[17u + category * 4u], size);
        }
    } else { atomicAdd(&counters.words[9], 1u); }
    output_masks[id.x] = membership;
    if categories.x > 0u { atomicAdd(&counters.words[10], categories.x); }
    if categories.y > 0u { atomicAdd(&counters.words[11], categories.y); }
    if categories.z > 0u { atomicAdd(&counters.words[12], categories.z); }
    if categories.w > 0u { atomicAdd(&counters.words[14], categories.w); }
}

// Stable compaction uses a separate resource group, keeping preparation below
// the default storage-binding limit. No atomic allocation determines draw order.
@group(2) @binding(0) var<storage, read> prefix_masks: array<vec4<u32>>;
@group(2) @binding(1) var<storage, read_write> prefix_offsets: array<vec4<u32>>;
@group(2) @binding(2) var<storage, read_write> prefix_blocks: array<vec4<u32>>;
@group(2) @binding(3) var<storage, read_write> prefix_indices: array<u32>;
var<workgroup> scan_values: array<vec4<u32>, 64>;

@compute @workgroup_size(64)
fn prefix_edges(@builtin(global_invocation_id) id: vec3<u32>,
    @builtin(local_invocation_index) lane: u32, @builtin(workgroup_id) group: vec3<u32>) {
    var counts = vec4(0u);
    if id.x < arrayLength(&prefix_masks) { counts = countOneBits(prefix_masks[id.x]); }
    scan_values[lane] = counts;
    workgroupBarrier();
    for (var step = 1u; step < 64u; step *= 2u) {
        var increment = vec4(0u);
        if lane >= step { increment = scan_values[lane - step]; }
        let previous = scan_values[lane];
        workgroupBarrier();
        scan_values[lane] = previous + increment;
        workgroupBarrier();
    }
    if id.x < arrayLength(&prefix_masks) { prefix_offsets[id.x] = scan_values[lane] - counts; }
    if lane == 63u { prefix_blocks[group.x] = scan_values[lane]; }
}

@compute @workgroup_size(64)
fn scatter_edges(@builtin(global_invocation_id) id: vec3<u32>,
    @builtin(local_invocation_index) lane: u32, @builtin(workgroup_id) group: vec3<u32>) {
    // The totals array is small and cacheable. Lanes sum disjoint prior blocks,
    // then reduce together to obtain this workgroup's deterministic base.
    var subtotal = vec4(0u);
    for (var block = 0u; block < group.x; block += 64u) {
        let index = block + lane;
        if index < group.x { subtotal += prefix_blocks[index]; }
    }
    scan_values[lane] = subtotal;
    workgroupBarrier();
    for (var step = 32u; step > 0u; step /= 2u) {
        if lane < step { scan_values[lane] += scan_values[lane + step]; }
        workgroupBarrier();
    }
    if id.x >= arrayLength(&prefix_masks) { return; }
    let base = scan_values[0] + prefix_offsets[id.x];
    let membership = prefix_masks[id.x];
    let capacity = arrayLength(&prefix_masks) * 6u;
    for (var category = 0u; category < 4u; category += 1u) {
        var local = 0u;
        let stride = select(6u, 2u, category == 3u);
        for (var bit = 0u; bit < stride; bit += 1u) {
            if (membership[category] & (1u << bit)) == 0u { continue; }
            prefix_indices[category * capacity + base[category] + local] = id.x * stride + bit;
            local += 1u;
        }
    }
}

@group(1) @binding(0) var<uniform> render_params: FrameParams;
@group(1) @binding(1) var<storage, read> triangles: array<CompactTriangle>;
@group(1) @binding(2) var<storage, read> parallel: array<ParallelInterval>;
@group(1) @binding(3) var<storage, read> bodies: array<BodyData>;
@group(1) @binding(4) var<storage, read> triangle_indices: array<u32>;
@group(1) @binding(5) var<storage, read> render_styles: array<vec4<f32>>;

// Ordinary triangles carry no temporal data, body lookup or barycentric mask.
struct RegularVertex {
    @builtin(position) position: vec4<f32>,
    @location(0) alpha: f32,
    @location(1) @interpolate(flat) flags: u32,
    @location(2) @interpolate(flat) first_plane: vec3<f32>,
    @location(3) @interpolate(flat) second_plane: vec3<f32>,
    @location(4) @interpolate(flat) rgb: vec3<f32>,
}
struct LobeVertex {
    @builtin(position) position: vec4<f32>,
    @location(0) alpha: f32,
    @location(1) weights: vec2<f32>,
    @location(2) @interpolate(flat) flags: u32,
    @location(3) @interpolate(flat) first_plane: vec3<f32>,
    @location(4) @interpolate(flat) second_plane: vec3<f32>,
    @location(5) @interpolate(flat) rgb: vec3<f32>,
}
fn load_triangle(instance: u32, category: u32) -> CompactTriangle {
    let index = triangle_indices[category * arrayLength(&triangles) + instance];
    return triangles[index];
}
fn regular_vertex_data(triangle: CompactTriangle, corner: u32) -> RegularVertex {
    let points = array<vec2<f32>, 3>(triangle.points.xy, triangle.points.zw, triangle.tail.xy);
    let alphas = array<f32, 3>(triangle.tail.z, triangle.tail.w, triangle.alpha);
    return RegularVertex(vec4(points[corner], 0.0, 1.0), alphas[corner],
        triangle.flags, vec3(triangle.first_normal, triangle.planes.x), triangle.planes.yzw,
        render_styles[triangle.flags >> 6u].rgb);
}
@vertex
fn regular_vertex(@builtin(vertex_index) corner: u32, @builtin(instance_index) instance: u32) -> RegularVertex {
    return regular_vertex_data(load_triangle(instance, 0u), corner);
}
@vertex
fn body_vertex(@builtin(vertex_index) corner: u32, @builtin(instance_index) instance: u32) -> RegularVertex {
    return regular_vertex_data(load_triangle(instance, 2u), corner);
}
@vertex
fn lobe_vertex(@builtin(vertex_index) corner: u32, @builtin(instance_index) instance: u32) -> LobeVertex {
    let value = regular_vertex_data(load_triangle(instance, 1u), corner);
    let weights = array<vec2<f32>, 3>(vec2(1.0, 0.0), vec2(0.0, 1.0), vec2(0.0));
    return LobeVertex(value.position, value.alpha, weights[corner], value.flags, value.first_plane, value.second_plane, value.rgb);
}
fn regular_coverage(point: vec2<f32>, flags: u32, first: vec3<f32>, second: vec3<f32>) -> f32 {
    var nearest = 1e6;
    if (flags & 8u) != 0u { nearest = min(nearest, abs(dot(first.xy, point) + first.z)); }
    if (flags & 16u) != 0u { nearest = min(nearest, abs(dot(second.xy, point) + second.z)); }
    return clamp(0.5 + nearest / max(render_params.viewport.z, render_params.viewport.w), 0.0, 1.0);
}
fn body_ink(v: RegularVertex) -> f32 {
    var nearest = 1e6;
    if (v.flags & 8u) != 0u { nearest = min(nearest, abs(dot(v.first_plane.xy, (v.position.xy * render_params.viewport.zw)) + v.first_plane.z)); }
    let body = bodies[v.flags >> 6u];
    let along = dot(body.along.xy, (v.position.xy * render_params.viewport.zw)) + body.along.z;
    let limits = select(body.attachment.xy, body.attachment.zw, (v.flags & 32u) != 0u);
    if !(along <= limits.x || along >= limits.y) {
        nearest = min(nearest, abs(dot(v.second_plane.xy, (v.position.xy * render_params.viewport.zw)) + v.second_plane.z));
    }
    return v.alpha * clamp(0.5 + nearest / max(render_params.viewport.z, render_params.viewport.w), 0.0, 1.0);
}
fn lobe_ink(v: LobeVertex) -> f32 {
    var alpha = v.alpha * regular_coverage((v.position.xy * render_params.viewport.zw), v.flags, v.first_plane, v.second_plane);
    let denominator = v.weights.x + v.weights.y;
    alpha *= select(1.0, v.weights.y / max(denominator, 1e-6), denominator > 1e-6);
    return alpha;
}
fn additive_output(alpha: f32, rgb: vec3<f32>) -> vec4<f32> {
    return vec4(max(rgb, vec3(1.0 / 255.0)) * alpha, alpha);
}
fn multiply_output(value: f32, rgb: vec3<f32>) -> vec4<f32> {
    let alpha = clamp(value, 0.0, 1.0);
    return vec4(mix(vec3(1.0), min(rgb, vec3(254.0 / 255.0)), alpha), 1.0);
}
@fragment fn regular_additive(v: RegularVertex) -> @location(0) vec4<f32> {
    return additive_output(v.alpha * regular_coverage((v.position.xy * render_params.viewport.zw), v.flags, v.first_plane, v.second_plane), v.rgb);
}
@fragment fn regular_multiply(v: RegularVertex) -> @location(0) vec4<f32> {
    return multiply_output(v.alpha * regular_coverage((v.position.xy * render_params.viewport.zw), v.flags, v.first_plane, v.second_plane), v.rgb);
}
@fragment fn lobe_additive(v: LobeVertex) -> @location(0) vec4<f32> { return additive_output(lobe_ink(v), v.rgb); }
@fragment fn lobe_multiply(v: LobeVertex) -> @location(0) vec4<f32> { return multiply_output(lobe_ink(v), v.rgb); }
@fragment fn body_additive(v: RegularVertex) -> @location(0) vec4<f32> { return additive_output(body_ink(v), v.rgb); }
@fragment fn body_multiply(v: RegularVertex) -> @location(0) vec4<f32> { return multiply_output(body_ink(v), v.rgb); }

struct ParallelVertex {
    @builtin(position) position: vec4<f32>,
    @location(0) @interpolate(flat) first: vec4<f32>,
    @location(1) @interpolate(flat) last: vec4<f32>,
    @location(2) @interpolate(flat) depths: vec4<f32>,
    @location(3) @interpolate(flat) setup: vec3<f32>,
    @location(4) @interpolate(flat) ink: vec4<f32>,
}
@vertex
fn parallel_vertex(@builtin(vertex_index) corner: u32, @builtin(instance_index) instance: u32) -> ParallelVertex {
    let index = triangle_indices[3u * arrayLength(&triangles) + instance];
    let value = parallel[index];
    let along = value.setup.xy;
    let across = vec2(-along.y, along.x);
    let points = array<vec2<f32>, 4>(value.first.xy, value.first.zw, value.last.xy, value.last.zw);
    var low = vec2(1e20);
    var high = vec2(-1e20);
    for (var i = 0u; i < 4u; i += 1u) {
        let local = vec2(dot(points[i], along), dot(points[i], across));
        low = min(low, local); high = max(high, local);
    }
    let pixel = max(render_params.viewport.z, render_params.viewport.w);
    let padding = max(render_params.style.x, pixel) * 0.5 + pixel * 0.75;
    low -= vec2(padding); high += vec2(padding);
    if TIGHT_PARALLEL_SUPPORT {
        // parallel_ink measures only across distance from the actual interval
        // middle line. Endpoint across movement broadens the old support, but
        // does not broaden this band. Intersecting with the old bounds retains
        // its finite support and leaves temporal cap shading unchanged.
        // Keep a subpixel arithmetic guard; extreme projected coordinates use
        // the original rectangle so cancellation cannot shrink visible ink.
        let coordinate_scale = max(max(abs(low.x), abs(low.y)), max(abs(high.x), abs(high.y)));
        if coordinate_scale < 65536.0 {
            let center = dot((value.first.xy + value.last.xy) * 0.5, across);
            let half = max(render_params.style.x, 0.05) * 0.5;
            let support = half + pixel * 0.5 + 0.125;
            low.y = max(low.y, center - support);
            high.y = min(high.y, center + support);
        }
    }
    let corners = array<vec2<f32>, 6>(vec2(0.0), vec2(1.0, 0.0), vec2(0.0, 1.0),
        vec2(0.0, 1.0), vec2(1.0, 0.0), vec2(1.0));
    let local = mix(low, high, corners[corner]);
    let point = along * local.x + across * local.y;
    return ParallelVertex(vec4(point.x / render_params.viewport.x * 2.0 - 1.0,
        1.0 - point.y / render_params.viewport.y * 2.0, 0.0, 1.0), value.first, value.last, value.depths, value.setup.xyz, render_styles[index / 2u]);
}
fn profile_integral(low: f32, high: f32, interval: u32) -> f32 {
    let moment = (high * high - low * low) * 0.5;
    return select(moment * render_params.temporal.x,
        (high - low - moment) * render_params.temporal.y, interval == 1u);
}
fn clip_positive(range: vec2<f32>, offset: f32, velocity: f32) -> vec2<f32> {
    if abs(velocity) < 1e-6 { return select(vec2(1.0, 0.0), range, offset >= 0.0); }
    let crossing = -offset / velocity;
    return select(vec2(range.x, min(range.y, crossing)), vec2(max(range.x, crossing), range.y), velocity > 0.0);
}
fn parallel_ink(v: ParallelVertex) -> f32 {
    let point = (v.position.xy * render_params.viewport.zw);
    let along = v.setup.xy;
    let across = vec2(-along.y, along.x);
    let a0 = v.first.xy; let b0 = v.first.zw;
    let a1 = v.last.xy; let b1 = v.last.zw;
    let half = max(render_params.style.x, 0.05) * 0.5;
    let u = dot(point - a0, along);
    var range = vec2(0.0, 1.0);
    range = clip_positive(range, u + half, -dot(a1 - a0, along));
    range = clip_positive(range, dot(b0 - a0, along) + half - u, dot(b1 - b0, along));
    let middle_a = (a0 + a1) * 0.5;
    let middle = (b0 + b1) * 0.5 - middle_a;
    let s = clamp(dot(point - middle_a, middle) / max(dot(middle, middle), 1e-8), 0.0, 1.0);
    let nearest = middle_a + middle * s;
    let pixel = max(render_params.viewport.z, render_params.viewport.w);
    let ceiling = select(1.0, min(1.0, 2.0 * half / pixel), pixel > 1.0);
    let coverage = clamp((half + pixel * 0.5 - abs(dot(point - nearest, across))) / pixel, 0.0, ceiling);
    let low = clamp(range.x, 0.0, 1.0); let high = clamp(range.y, 0.0, 1.0);
    if high <= low { return 0.0; }
    let depth = mix(mix(v.depths.x, v.depths.y, s), mix(v.depths.z, v.depths.w, s), 0.5);
    return coverage * profile_integral(low, high, u32(v.setup.z)) * depth * v.ink.a;
}
@fragment fn parallel_additive(v: ParallelVertex) -> @location(0) vec4<f32> {
    let alpha = parallel_ink(v);
    return vec4(max(v.ink.rgb, vec3(1.0 / 255.0)) * alpha, alpha);
}
@fragment fn parallel_multiply(v: ParallelVertex) -> @location(0) vec4<f32> {
    let alpha = clamp(parallel_ink(v), 0.0, 1.0);
    return vec4(mix(vec3(1.0), min(v.ink.rgb, vec3(254.0 / 255.0)), alpha), 1.0);
}
