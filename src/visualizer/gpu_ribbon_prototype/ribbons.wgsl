// PROTOTYPE: one static vertex/index upload, one new projected ring slot/frame.
// The render pass reconstructs two time intervals without CPU ribbon geometry.
struct FrameParams {
    rotation: array<vec4<f32>, 3>,
    viewport: vec4<f32>,
    camera: vec4<f32>,
    pose: vec4<f32>,
    style: vec4<f32>,
    tint: vec4<f32>,
    history: vec4<u32>,
}

@group(0) @binding(0) var<uniform> project_params: FrameParams;
@group(0) @binding(1) var<storage, read> vertices: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> projection_ring: array<vec4<f32>>;

@compute @workgroup_size(64)
fn project_vertices(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= project_params.history.x { return; }
    let p = vertices[id.x].xyz;
    let rotated = project_params.rotation[0].xyz * p.x
        + project_params.rotation[1].xyz * p.y
        + project_params.rotation[2].xyz * p.z;
    let raw_depth = rotated.z + project_params.pose.x;
    let depth = max(raw_depth, project_params.camera.w);
    let perspective = project_params.camera.z / depth;
    let screen = project_params.camera.xy
        + vec2(rotated.x, -rotated.y) * perspective + project_params.pose.yz;
    let radius = max(project_params.pose.w, 1e-6);
    let fraction = clamp((depth - (project_params.pose.x - radius)) / (radius * 2.0), 0.0, 1.0);
    projection_ring[project_params.history.y * project_params.history.x + id.x]
        = vec4(screen, 1.0 - fraction * 0.5, select(0.0, 1.0, raw_depth > project_params.camera.w));
}

@group(1) @binding(0) var<uniform> params: FrameParams;
@group(1) @binding(1) var<storage, read> edges: array<vec2<u32>>;
@group(1) @binding(2) var<storage, read> history: array<vec4<f32>>;

struct RibbonVertex {
    @builtin(position) position: vec4<f32>,
    @location(0) @interpolate(flat) first: vec4<f32>,
    @location(1) @interpolate(flat) last: vec4<f32>,
    @location(2) @interpolate(flat) depths: vec4<f32>,
    @location(3) @interpolate(flat) interval: u32,
    @location(4) @interpolate(flat) fast_mode: u32,
    @location(5) ink: vec4<f32>,
    @location(6) @interpolate(flat) support_a: vec4<f32>,
    @location(7) @interpolate(flat) support_b: vec4<f32>,
}

fn cross2(a: vec2<f32>, b: vec2<f32>) -> f32 { return a.x * b.y - a.y * b.x; }

fn interval_vertex(corner: u32, instance: u32) -> RibbonVertex {
    let edge = edges[instance / 2u];
    let interval = instance % 2u;
    let first_slot = (params.history.z + interval) % 3u;
    let last_slot = (first_slot + 1u) % 3u;
    let a0 = history[first_slot * params.history.x + edge.x];
    let b0 = history[first_slot * params.history.x + edge.y];
    let a1 = history[last_slot * params.history.x + edge.x];
    let b1 = history[last_slot * params.history.x + edge.y];
    var result: RibbonVertex;
    result.first = vec4(a0.xy, b0.xy);
    result.last = vec4(a1.xy, b1.xy);
    result.depths = vec4(a0.z, b0.z, a1.z, b1.z);
    result.interval = interval;
    result.fast_mode = 1u;
    result.ink = vec4(0.0);
    result.support_a = vec4(0.0);
    result.support_b = vec4(0.0);
    if min(min(a0.w, b0.w), min(a1.w, b1.w)) < 0.5 {
        result.position = vec4(-2.0, -2.0, 0.0, 1.0);
        return result;
    }
    let e0 = b0.xy - a0.xy;
    let e1 = b1.xy - a1.xy;
    // Align the support to its dominant spatial extent. Tiny edges moving far
    // along a diagonal must align to motion, otherwise an edge-aligned box
    // rasterizes an enormous mostly-empty rectangle around the real sweep.
    var along = e0 + e1;
    let da = (a1.xy - a0.xy) * 2.0;
    let db = (b1.xy - b0.xy) * 2.0;
    if dot(da, da) > dot(along, along) { along = da; }
    if dot(db, db) > dot(along, along) { along = db; }
    if dot(along, along) < 1e-8 { along = e0; }
    if dot(along, along) < 1e-8 { along = vec2(1.0, 0.0); }
    along = normalize(along);
    let across = vec2(-along.y, along.x);
    let points = array<vec2<f32>, 4>(a0.xy, b0.xy, a1.xy, b1.xy);
    var low = vec2(1e20);
    var high = vec2(-1e20);
    for (var i = 0u; i < 4u; i += 1u) {
        let local = vec2(dot(points[i], along), dot(points[i], across));
        low = min(low, local);
        high = max(high, local);
    }
    let padding = max(params.style.x, 1.0) * 0.5 + 0.75;
    low -= vec2(padding);
    high += vec2(padding);
    let corners = array<vec2<f32>, 6>(
        vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(0.0, 1.0),
        vec2(0.0, 1.0), vec2(1.0, 0.0), vec2(1.0, 1.0),
    );
    let local = mix(low, high, corners[corner]);
    var point = along * local.x + across * local.y;
    if (params.history.w & 1u) != 0u {
        let obb = array<vec2<f32>, 4>(
            along * low.x + across * low.y,
            along * high.x + across * low.y,
            along * high.x + across * high.y,
            along * low.x + across * high.y,
        );
        let tightened = tight_support(points, obb, padding);
        // Keep the same four corner identities as the legacy OBB path so an
        // indexed draw can reuse corners without changing flat ribbon data.
        let indices = array<u32, 6>(0u, 1u, 3u, 3u, 1u, 2u);
        point = tightened[indices[corner]];
    }
    result.position = vec4(point.x / params.viewport.x * 2.0 - 1.0,
        1.0 - point.y / params.viewport.y * 2.0, 0.0, 1.0);
    return result;
}

@vertex
fn ribbon_vertex(
    @builtin(vertex_index) corner: u32,
    @builtin(instance_index) instance: u32,
) -> RibbonVertex {
    return interval_vertex(corner, instance);
}

@vertex
fn ribbon_indexed_vertex(
    @builtin(vertex_index) corner: u32,
    @builtin(instance_index) instance: u32,
) -> RibbonVertex {
    let legacy_corners = array<u32, 4>(0u, 1u, 2u, 5u);
    return interval_vertex(legacy_corners[corner], instance);
}

fn support_edge(a: vec2<f32>, b: vec2<f32>, enabled: bool) -> vec4<f32> {
    let edge = b - a;
    let inverse_length = inverseSqrt(max(dot(edge, edge), 1e-12));
    let normal = vec2(-edge.y, edge.x) * inverse_length;
    return vec4(normal, -dot(normal, a), select(0.0, 1.0, enabled));
}

// FAST GEOMETRY: prepare once per edge, then select the six triangle corners.
// Keeping setup separate also allows the next experiment to move it to compute.
struct FastEdge {
    a0: vec4<f32>, b0: vec4<f32>, am: vec4<f32>, bm: vec4<f32>,
    a1: vec4<f32>, b1: vec4<f32>,
    direction: vec2<f32>, normal: vec2<f32>,
    old_offset: vec2<f32>, new_offset: vec2<f32>,
    peaks: vec2<f32>, weights: vec2<f32>,
    // For each body side, attachments cover s <= x or s >= y.
    // xy is the negative-normal side, zw the positive-normal side.
    attachment: vec4<f32>,
    half: f32, width: f32, visible: u32, parallel: u32,
}

fn empty_vertex() -> RibbonVertex {
    var result: RibbonVertex;
    result.position = vec4(-2.0, -2.0, 0.0, 1.0);
    result.first = vec4(0.0);
    result.last = vec4(0.0);
    result.depths = vec4(0.0);
    result.interval = 0u;
    result.fast_mode = 0u;
    result.ink = vec4(0.0);
    result.support_a = vec4(0.0);
    result.support_b = vec4(0.0);
    return result;
}

// Intersection parameters on the two finite segments; z marks validity.
fn segment_hit(a: vec2<f32>, b: vec2<f32>, c: vec2<f32>, d: vec2<f32>) -> vec3<f32> {
    let e = b - a;
    let f = d - c;
    let determinant = cross2(e, f);
    if abs(determinant) < 1e-6 { return vec3(0.0); }
    let offset = c - a;
    let u = cross2(offset, f) / determinant;
    let v = cross2(offset, e) / determinant;
    return vec3(u, v, select(0.0, 1.0, u >= 0.0 && u <= 1.0 && v >= 0.0 && v <= 1.0));
}

fn attach_wing(attachment: vec4<f32>, offset: vec2<f32>, half: f32,
    outer_a: vec2<f32>, outer_b: vec2<f32>, middle_a: vec2<f32>, direction: vec2<f32>, inverse_length: f32) -> vec4<f32> {
    var result = attachment;
    let full = min(abs(offset.x), abs(offset.y)) > half && offset.x * offset.y > 0.0;
    if full {
        if offset.x < 0.0 { result.x = 1.0; } else { result.z = 1.0; }
        return result;
    }
    for (var endpoint = 0u; endpoint < 2u; endpoint += 1u) {
        let h = offset[endpoint];
        if abs(h) <= half { continue; }
        let side = select(-1.0, 1.0, h > 0.0);
        let fraction = clamp((side * half - offset.x) / (offset.y - offset.x), 0.0, 1.0);
        let intersection = mix(outer_a, outer_b, fraction);
        let along = clamp(dot(intersection - middle_a, direction) * inverse_length, 0.0, 1.0);
        if side < 0.0 {
            if endpoint == 0u { result.x = max(result.x, along); }
            else { result.y = min(result.y, along); }
        } else {
            if endpoint == 0u { result.z = max(result.z, along); }
            else { result.w = min(result.w, along); }
        }
    }
    return result;
}

fn prepare_fast_edge(edge_index: u32) -> FastEdge {
    let edge = edges[edge_index];
    let oldest = params.history.z;
    let middle = (oldest + 1u) % 3u;
    let newest = (oldest + 2u) % 3u;
    var value: FastEdge;
    value.a0 = history[oldest * params.history.x + edge.x];
    value.b0 = history[oldest * params.history.x + edge.y];
    value.am = history[middle * params.history.x + edge.x];
    value.bm = history[middle * params.history.x + edge.y];
    value.a1 = history[newest * params.history.x + edge.x];
    value.b1 = history[newest * params.history.x + edge.y];
    value.visible = select(0u, 1u,
        min(min(min(value.a0.w, value.b0.w), min(value.am.w, value.bm.w)), min(value.a1.w, value.b1.w)) >= 0.5);
    value.width = max(params.style.x, 0.05);
    value.half = max(value.width * 0.5, 0.5);
    var direction = value.bm.xy - value.am.xy;
    if dot(direction, direction) < 1e-10 { direction = value.b0.xy - value.a0.xy; }
    if dot(direction, direction) < 1e-10 { direction = vec2(1.0, 0.0); }
    value.direction = normalize(direction);
    value.normal = vec2(-value.direction.y, value.direction.x);
    value.old_offset = vec2(dot(value.a0.xy - value.am.xy, value.normal), dot(value.b0.xy - value.bm.xy, value.normal));
    value.new_offset = vec2(dot(value.a1.xy - value.am.xy, value.normal), dot(value.b1.xy - value.bm.xy, value.normal));
    let travel_a = length(value.am.xy - value.a0.xy) + length(value.a1.xy - value.am.xy);
    let travel_b = length(value.bm.xy - value.b0.xy) + length(value.b1.xy - value.bm.xy);
    value.parallel = select(0u, 1u,
        max(max(abs(value.old_offset.x), abs(value.old_offset.y)), max(abs(value.new_offset.x), abs(value.new_offset.y))) <= value.half
        && max(travel_a, travel_b) > value.width);
    let remaining = value.width / (value.width + max(travel_a, travel_b));
    var retention = sqrt(remaining);
    if abs(params.style.z - 0.5) > 1e-6 { retention = pow(remaining, params.style.z); }
    let peak = params.style.y * (params.style.w + (1.0 - params.style.w) * retention);
    value.peaks = vec2(peak * value.am.z, peak * value.bm.z);
    // Each endpoint can reverse independently. Sharing its peak between the two
    // wings avoids a doubled seam without changing the one middle body.
    value.weights = vec2(
        select(1.0, 0.5, abs(value.old_offset.x) > value.half && abs(value.new_offset.x) > value.half && value.old_offset.x * value.new_offset.x > 0.0),
        select(1.0, 0.5, abs(value.old_offset.y) > value.half && abs(value.new_offset.y) > value.half && value.old_offset.y * value.new_offset.y > 0.0));
    let inverse_length = inverseSqrt(max(dot(value.bm.xy - value.am.xy, value.bm.xy - value.am.xy), 1e-10));
    value.attachment = attach_wing(vec4(-1.0, 2.0, -1.0, 2.0), value.old_offset, value.half,
        value.a0.xy, value.b0.xy, value.am.xy, value.direction, inverse_length);
    value.attachment = attach_wing(value.attachment, value.new_offset, value.half,
        value.a1.xy, value.b1.xy, value.am.xy, value.direction, inverse_length);
    return value;
}

fn fast_triangle(corner: u32, points: array<vec2<f32>, 3>, alphas: vec3<f32>, free_edges: u32, mode: u32) -> RibbonVertex {
    var result = empty_vertex();
    let point = points[corner % 3u];
    result.position = vec4(point.x / params.viewport.x * 2.0 - 1.0,
        1.0 - point.y / params.viewport.y * 2.0, 0.0, 1.0);
    let weights = array<vec3<f32>, 3>(vec3(1.0, 0.0, 0.0), vec3(0.0, 1.0, 0.0), vec3(0.0, 0.0, 1.0));
    result.ink = vec4(weights[corner % 3u], alphas[corner % 3u]);
    result.fast_mode = mode;
    if (free_edges & 1u) != 0u { result.support_a = support_edge(points[0], points[1], true); }
    if (free_edges & 2u) != 0u {
        if result.support_a.w > 0.5 { result.support_b = support_edge(points[1], points[2], true); }
        else { result.support_a = support_edge(points[1], points[2], true); }
    }
    if (free_edges & 4u) != 0u {
        if result.support_a.w > 0.5 { result.support_b = support_edge(points[2], points[0], true); }
        else { result.support_a = support_edge(points[2], points[0], true); }
    }
    return result;
}

fn fast_wing(corner: u32, outer_a: vec2<f32>, outer_b: vec2<f32>, offset: vec2<f32>, edge: FastEdge) -> RibbonVertex {
    let triangle = corner / 3u;
    let local = corner % 3u;
    let peaks = edge.peaks * edge.weights;
    let full = min(abs(offset.x), abs(offset.y)) > edge.half && offset.x * offset.y > 0.0;
    if !full {
        // Opposite sides (or just one cleared endpoint): the two outer pieces
        // terminate at their actual intersections with the middle-body support.
        // This is a butterfly made of two lobes, not a large solver rectangle.
        let h = offset[triangle];
        if abs(h) <= edge.half { return empty_vertex(); }
        let side = select(-1.0, 1.0, h > 0.0);
        let base_a = edge.am.xy + edge.normal * edge.half * side;
        let base_b = edge.bm.xy + edge.normal * edge.half * side;
        let fraction = clamp((side * edge.half - offset.x) / (offset.y - offset.x), 0.0, 1.0);
        let hit = mix(outer_a, outer_b, fraction);
        let segment = base_b - base_a;
        let along = clamp(dot(hit - base_a, segment) / max(dot(segment, segment), 1e-10), 0.0, 1.0);
        let intersection = mix(base_a, base_b, along);
        let outer = select(outer_a, outer_b, triangle == 1u);
        let base = select(base_a, base_b, triangle == 1u);
        let alpha = select(peaks.x, peaks.y, triangle == 1u);
        return fast_triangle(local, array<vec2<f32>, 3>(outer, base, intersection),
            vec3(0.0, alpha, mix(peaks.x, peaks.y, along)), 1u, 2u);
    }
    let side = select(-1.0, 1.0, offset.x + offset.y > 0.0);
    let base_a = edge.am.xy + edge.normal * edge.half * side;
    let base_b = edge.bm.xy + edge.normal * edge.half * side;
    let points = array<vec2<f32>, 4>(outer_a, outer_b, base_b, base_a);
    let alphas = vec4(0.0, 0.0, peaks.y, peaks.x);
    let hit = segment_hit(outer_a, base_a, outer_b, base_b);
    if hit.z > 0.5 {
        // Reversed endpoint trajectories make the OTHER bow-tie orientation.
        // Its lobes have an interpolated intersection value, no inverse solve.
        let intersection = mix(outer_a, base_a, hit.x);
        let alpha = (peaks.x * hit.x + peaks.y * hit.y) * 0.5;
        if triangle == 0u {
            return fast_triangle(local, array<vec2<f32>, 3>(outer_a, outer_b, intersection), vec3(0.0, 0.0, alpha), 2u | 4u, 0u);
        }
        return fast_triangle(local, array<vec2<f32>, 3>(base_a, base_b, intersection), vec3(peaks.x, peaks.y, alpha), 2u | 4u, 0u);
    }
    // A concave quad needs the internal diagonal through its reflex corner.
    let turns = vec4(cross2(points[1] - points[0], points[2] - points[1]),
        cross2(points[2] - points[1], points[3] - points[2]),
        cross2(points[3] - points[2], points[0] - points[3]),
        cross2(points[0] - points[3], points[1] - points[0]));
    let area = cross2(points[1] - points[0], points[2] - points[0])
        + cross2(points[2] - points[0], points[3] - points[0]);
    let opposite_diagonal = turns.x * area < 0.0 || turns.z * area < 0.0;
    var indices = array<u32, 3>(0u, 1u, 2u);
    var free = 2u;
    if opposite_diagonal {
        indices = array<u32, 3>(0u, 1u, 3u);
        free = 4u;
        if triangle == 1u { indices = array<u32, 3>(1u, 2u, 3u); free = 1u; }
    } else if triangle == 1u {
        indices = array<u32, 3>(0u, 2u, 3u);
        free = 4u;
    }
    return fast_triangle(local, array<vec2<f32>, 3>(points[indices[0]], points[indices[1]], points[indices[2]]),
        vec3(alphas[indices[0]], alphas[indices[1]], alphas[indices[2]]), free, 0u);
}

fn fast_corner(corner: u32, edge: FastEdge) -> RibbonVertex {
    if edge.visible == 0u { return empty_vertex(); }
    let section = corner / 6u;
    let local = corner % 6u;
    if section == 0u { return fast_wing(local, edge.a0.xy, edge.b0.xy, edge.old_offset, edge); }
    if section == 2u { return fast_wing(local, edge.a1.xy, edge.b1.xy, edge.new_offset, edge); }
    let points = array<vec2<f32>, 4>(edge.am.xy - edge.normal * edge.half, edge.bm.xy - edge.normal * edge.half,
        edge.bm.xy + edge.normal * edge.half, edge.am.xy + edge.normal * edge.half);
    var result: RibbonVertex;
    if local < 3u {
        result = fast_triangle(local, array<vec2<f32>, 3>(points[0], points[1], points[2]), vec3(edge.peaks.x, edge.peaks.y, edge.peaks.y), 2u, 4u);
        result.support_b = support_edge(points[0], points[1], true);
        result.interval = 0u;
    } else {
        result = fast_triangle(local % 3u, array<vec2<f32>, 3>(points[0], points[2], points[3]), vec3(edge.peaks.x, edge.peaks.y, edge.peaks.x), 4u, 4u);
        result.support_b = support_edge(points[2], points[3], true);
        result.interval = 1u;
    }
    // Body pixels have along coordinates in [0,1]. Most sides are either
    // completely free or completely covered by wings; specialize those cases
    // here so they use the ordinary compact shader and need no attachment
    // lookup/along-coordinate calculation per fragment.
    let limits = select(edge.attachment.xy, edge.attachment.zw, result.interval == 1u);
    if limits.x <= 0.0 && limits.y >= 1.0 {
        result.fast_mode = 0u;
        return result;
    }
    if limits.x >= 1.0 || limits.y <= 0.0 || limits.x >= limits.y {
        result.fast_mode = 0u;
        result.support_b = vec4(0.0);
        return result;
    }
    let segment = edge.bm.xy - edge.am.xy;
    let along_coefficient = segment / max(dot(segment, segment), 1e-10);
    result.first = edge.attachment;
    result.last = vec4(along_coefficient, -dot(along_coefficient, edge.am.xy), 0.0);
    return result;
}

@vertex
fn ribbon_fast_vertex(@builtin(vertex_index) corner: u32, @builtin(instance_index) edge_index: u32) -> RibbonVertex {
    let edge = prepare_fast_edge(edge_index);
    if edge.parallel != 0u {
        if corner >= 12u { return empty_vertex(); }
        var result = interval_vertex(corner % 6u, edge_index * 2u + corner / 6u);
        result.fast_mode = 3u;
        return result;
    }
    return fast_corner(corner, edge);
}

// Integral of the SINGLE shutter tent, whose vertices are [-2:0,-1:1,0:0].
// The two intervals each integrate to 0.5 at rest. Splitting never introduces
// two full-strength bodies or a double-strength shared temporal boundary.
fn profile_integral(low: f32, high: f32, interval: u32) -> f32 {
    let moment = (high * high - low * low) * 0.5;
    return select(moment, high - low - moment, interval == 1u);
}

fn clip_positive(range: vec2<f32>, offset: f32, velocity: f32) -> vec2<f32> {
    if abs(velocity) < 1e-6 {
        return select(vec2(1.0, 0.0), range, offset >= 0.0);
    }
    let crossing = -offset / velocity;
    return select(vec2(range.x, min(range.y, crossing)),
        vec2(max(range.x, crossing), range.y), velocity > 0.0);
}

fn stationary_ink(v: RibbonVertex, point: vec2<f32>, along: vec2<f32>) -> f32 {
    let a0 = v.first.xy;
    let b0 = v.first.zw;
    let a1 = v.last.xy;
    let b1 = v.last.zw;
    let relative = point - a0;
    let u = dot(relative, along);
    let across = vec2(-along.y, along.x);
    let width = max(params.style.x, 0.05);
    let half = width * 0.5;
    // Linear cap-time clipping also preserves a wire moving along its length.
    var range = vec2(0.0, 1.0);
    range = clip_positive(range, u + half, -dot(a1 - a0, along));
    range = clip_positive(range, dot(b0 - a0, along) + half - u, dot(b1 - b0, along));
    let middle_a = (a0 + a1) * 0.5;
    let middle_b = (b0 + b1) * 0.5;
    let middle = middle_b - middle_a;
    let s = clamp(dot(point - middle_a, middle) / max(dot(middle, middle), 1e-8), 0.0, 1.0);
    let nearest = middle_a + middle * s;
    let coverage = clamp(half + 0.5 - abs(dot(point - nearest, across)), 0.0, 1.0);
    let low = clamp(range.x, 0.0, 1.0);
    let high = clamp(range.y, 0.0, 1.0);
    if high <= low { return 0.0; }
    let depth = mix(mix(v.depths.x, v.depths.y, s), mix(v.depths.z, v.depths.w, s), 0.5);
    return coverage * profile_integral(low, high, v.interval) * depth;
}

fn root_ink(v: RibbonVertex, point: vec2<f32>, time: f32) -> f32 {
    let a0 = v.first.xy;
    let b0 = v.first.zw;
    let a1 = v.last.xy;
    let b1 = v.last.zw;
    let t = clamp(time, 0.0, 1.0);
    let a = mix(a0, a1, t);
    let b = mix(b0, b1, t);
    let e = b - a;
    let edge_length = length(e);
    if edge_length < 1e-5 { return 0.0; }
    let s = dot(point - a, e) / (edge_length * edge_length);
    let along = clamp(s, 0.0, 1.0);
    let velocity = mix(a1 - a0, b1 - b0, along);
    let movement = abs(cross2(e / edge_length, velocity));
    let width = max(params.style.x, 0.05);
    let reach = (width * 0.5 + 0.5) / max(movement, 1e-5);
    let low = max(0.0, time - reach);
    let high = min(1.0, time + reach);
    if high <= low { return 0.0; }
    // Reject extrapolated endpoint/cap roots that are outside the real stroke.
    let nearest = a + e * along;
    var coverage = 1.0;
    if s < 0.0 || s > 1.0 || time < 0.0 || time > 1.0 {
        coverage = clamp(width * 0.5 + 0.5 - distance(point, nearest), 0.0, 1.0);
    }
    // Match the current artistic speed fade approximately, using transverse
    // movement. The profile integral supplies temporal normalization; the gain
    // retains luminous trails instead of imposing a physical 1/speed falloff.
    let remaining = width / (width + movement * 2.0);
    var retention = sqrt(remaining);
    if abs(params.style.z - 0.5) > 1e-6 { retention = pow(remaining, params.style.z); }
    let fade = params.style.w + (1.0 - params.style.w) * retention;
    let gain = fade / min(1.0, reach * 2.0);
    let depth = mix(mix(v.depths.x, v.depths.y, along), mix(v.depths.z, v.depths.w, along), t);
    return coverage * profile_integral(low, high, v.interval) * gain * depth;
}

fn ribbon_alpha(v: RibbonVertex) -> f32 {
    let point = v.position.xy;
    let a = v.first.xy;
    let e = v.first.zw - a;
    let movement = v.last.xy - a;
    let delta_e = (v.last.zw - v.last.xy) - e;
    let middle_e = e + delta_e * 0.5;
    var along = middle_e;
    if length(along) < 1e-5 { along = e; }
    if length(along) < 1e-5 { along = vec2(1.0, 0.0); }
    along = normalize(along);
    let across_movement = max(abs(cross2(along, movement)),
        abs(cross2(along, movement + delta_e)));
    if across_movement < max(params.style.x * 0.25, 0.1) && dot(e, e + delta_e) >= 0.0 {
        return stationary_ink(v, point, along) * params.style.y;
    }
    // Invert the bilinear patch P(s,t)=A+s*E+t*V+s*t*dE. A rotating
    // sweep can have two roots; both are distinct shutter-time contributions.
    let relative = point - a;
    let c = cross2(relative, e);
    let b = cross2(relative, delta_e) - cross2(movement, e);
    let quadratic = -cross2(movement, delta_e);
    var ink = 0.0;
    if abs(quadratic) < 1e-5 {
        if abs(b) > 1e-5 { ink = root_ink(v, point, -c / b); }
    } else {
        let discriminant = b * b - 4.0 * quadratic * c;
        if discriminant >= 0.0 {
            let root = sqrt(discriminant);
            let t0 = (-b - root) / (2.0 * quadratic);
            let t1 = (-b + root) / (2.0 * quadratic);
            ink = root_ink(v, point, t0);
            if abs(t1 - t0) > 1e-4 { ink += root_ink(v, point, t1); }
        }
    }
    return ink * params.style.y;
}

@fragment
fn ribbon_additive(v: RibbonVertex) -> @location(0) vec4<f32> {
    let alpha = ribbon_alpha(v);
    return vec4(max(params.tint.rgb, vec3(1.0 / 255.0)) * alpha, alpha);
}

@fragment
fn ribbon_multiply(v: RibbonVertex) -> @location(0) vec4<f32> {
    let alpha = clamp(ribbon_alpha(v), 0.0, 1.0);
    return vec4(mix(vec3(1.0), min(params.tint.rgb, vec3(254.0 / 255.0)), alpha), 1.0);
}

fn fast_alpha(v: RibbonVertex) -> f32 {
    if v.fast_mode == 1u { return ribbon_alpha(v); }
    if v.fast_mode == 3u {
        var direction = (v.first.zw - v.first.xy) + (v.last.zw - v.last.xy);
        if dot(direction, direction) < 1e-10 { direction = vec2(1.0, 0.0); }
        return stationary_ink(v, v.position.xy, normalize(direction)) * params.style.y;
    }
    var nearest = 1e6;
    if v.support_a.w > 0.5 { nearest = min(nearest, abs(dot(v.support_a.xy, v.position.xy) + v.support_a.z)); }
    var second_support = v.support_b.w > 0.5;
    if v.fast_mode == 4u {
        let along = dot(v.last.xy, v.position.xy) + v.last.z;
        let limits = select(v.first.xy, v.first.zw, v.interval == 1u);
        second_support = !(along <= limits.x || along >= limits.y);
    }
    if second_support { nearest = min(nearest, abs(dot(v.support_b.xy, v.position.xy) + v.support_b.z)); }
    var alpha = v.ink.a;
    if v.fast_mode == 2u {
        let denominator = v.ink.x + v.ink.y;
        alpha *= select(1.0, v.ink.y / max(denominator, 1e-6), denominator > 1e-6);
    }
    return alpha * clamp(0.5 + nearest, 0.0, 1.0);
}

@fragment
fn ribbon_fast_additive(v: RibbonVertex) -> @location(0) vec4<f32> {
    let alpha = fast_alpha(v);
    return vec4(max(params.tint.rgb, vec3(1.0 / 255.0)) * alpha, alpha);
}

@fragment
fn ribbon_fast_multiply(v: RibbonVertex) -> @location(0) vec4<f32> {
    let alpha = clamp(fast_alpha(v), 0.0, 1.0);
    return vec4(mix(vec3(1.0), min(params.tint.rgb, vec3(254.0 / 255.0)), alpha), 1.0);
}
