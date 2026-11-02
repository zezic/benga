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
    // Retain any visible historical interval when the current pose crosses the
    // near plane. Freeze missing poses at the nearest visible sample; this is
    // conservative history rejection, rather than connecting a hidden endpoint.
    let valid_old = min(value.a0.w, value.b0.w) >= 0.5;
    let valid_mid = min(value.am.w, value.bm.w) >= 0.5;
    let valid_new = min(value.a1.w, value.b1.w) >= 0.5;
    value.visible = select(0u, 1u, valid_old || valid_mid || valid_new);
    if !valid_mid {
        value.am = select(value.a1, value.a0, valid_old);
        value.bm = select(value.b1, value.b0, valid_old);
    }
    if !valid_old { value.a0 = value.am; value.b0 = value.bm; }
    if !valid_new { value.a1 = value.am; value.b1 = value.bm; }
    value.width = max(params.style.x, 0.05);
    // Cover at least one model-target pixel so thin stationary strokes survive
    // coarse targets. Body energy is compensated below; broad wings keep ink.
    value.half = max(value.width, max(params.viewport.z, params.viewport.w)) * 0.5;
    var direction = value.bm.xy - value.am.xy;
    if dot(direction, direction) < 1e-10 { direction = value.b0.xy - value.a0.xy; }
    if dot(direction, direction) < 1e-10 { direction = vec2(1.0, 0.0); }
    value.direction = normalize(direction);
    value.normal = vec2(-value.direction.y, value.direction.x);
    value.old_offset = vec2(dot(value.a0.xy - value.am.xy, value.normal), dot(value.b0.xy - value.bm.xy, value.normal));
    value.new_offset = vec2(dot(value.a1.xy - value.am.xy, value.normal), dot(value.b1.xy - value.bm.xy, value.normal));
    let travel_a = length(value.am.xy - value.a0.xy) + length(value.a1.xy - value.am.xy);
    let travel_b = length(value.bm.xy - value.b0.xy) + length(value.b1.xy - value.bm.xy);
    // The wider raster footprint only preserves subpixel stroke coverage.
    // Classify motion against the physical stroke: coarsening the target must
    // not turn an ordinary sideways sweep into the parallel cap-time shader.
    let physical_half = max(value.width * 0.5, 0.5);
    value.parallel = select(0u, 1u,
        max(max(abs(value.old_offset.x), abs(value.old_offset.y)), max(abs(value.new_offset.x), abs(value.new_offset.y))) <= physical_half
        && max(travel_a, travel_b) > value.width);
    let remaining = value.width / (value.width + max(travel_a, travel_b));
    var retention = sqrt(remaining);
    if abs(params.style.z - 0.5) > 1e-6 { retention = pow(remaining, params.style.z); }
    let peak = edge_styles[edge_index].a * (params.style.w + (1.0 - params.style.w) * retention);
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
    if section == 0u {
        var weighted = edge;
        weighted.peaks *= params.temporal.x;
        return fast_wing(local, edge.a0.xy, edge.b0.xy, edge.old_offset, weighted);
    }
    if section == 2u {
        var weighted = edge;
        weighted.peaks *= params.temporal.y;
        return fast_wing(local, edge.a1.xy, edge.b1.xy, edge.new_offset, weighted);
    }
    let body_peak = edge.peaks * min(1.0, max(edge.width, 1.0) / (2.0 * edge.half));
    let points = array<vec2<f32>, 4>(edge.am.xy - edge.normal * edge.half, edge.bm.xy - edge.normal * edge.half,
        edge.bm.xy + edge.normal * edge.half, edge.am.xy + edge.normal * edge.half);
    var result: RibbonVertex;
    if local < 3u {
        result = fast_triangle(local, array<vec2<f32>, 3>(points[0], points[1], points[2]), vec3(body_peak.x, body_peak.y, body_peak.y), 2u, 4u);
        result.support_b = support_edge(points[0], points[1], true);
        result.interval = 0u;
    } else {
        result = fast_triangle(local % 3u, array<vec2<f32>, 3>(points[0], points[2], points[3]), vec3(body_peak.x, body_peak.y, body_peak.x), 4u, 4u);
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

