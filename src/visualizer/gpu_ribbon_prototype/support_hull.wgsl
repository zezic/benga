// Optional full-resolution support tightening for the inverse-coverage path.
// `obb` must be the current conservative, padded rectangle in CYCLIC order.
// Return the same rectangle whenever a stable, smaller padded hull is unavailable.

fn support_cross(a: vec2<f32>, b: vec2<f32>) -> f32 {
    return a.x * b.y - a.y * b.x;
}

fn support_polygon_area(points: array<vec2<f32>, 4>, count: u32) -> f32 {
    var twice_area = 0.0;
    let origin = points[0];
    for (var index = 1u; index + 1u < count; index += 1u) {
        twice_area += support_cross(points[index] - origin, points[index + 1u] - origin);
    }
    return abs(twice_area) * 0.5;
}

fn tight_support(
    points: array<vec2<f32>, 4>,
    obb: array<vec2<f32>, 4>,
    padding: f32,
) -> array<vec2<f32>, 4> {
    if padding < 0.0 { return obb; }
    let obb_area = support_polygon_area(obb, 4u);
    if !(obb_area > 1e-5 && obb_area < 1e18) { return obb; }

    // Gift wrapping needs no sorting/storage for this four-point input. Choose
    // the most clockwise next edge, leaving every other point on its left.
    var start = 0u;
    for (var index = 1u; index < 4u; index += 1u) {
        let left = points[index].x < points[start].x;
        let lower = points[index].x == points[start].x && points[index].y < points[start].y;
        if left || lower { start = index; }
    }
    var hull: array<vec2<f32>, 4>;
    var count = 0u;
    var current = start;
    for (var step = 0u; step < 4u; step += 1u) {
        hull[count] = points[current];
        count += 1u;
        var next_index = (current + 1u) % 4u;
        for (var candidate = 0u; candidate < 4u; candidate += 1u) {
            if candidate == current { continue; }
            let edge = points[next_index] - points[current];
            let delta = points[candidate] - points[current];
            let edge_length_squared = dot(edge, edge);
            let delta_length_squared = dot(delta, delta);
            let turn = support_cross(edge, delta);
            if edge_length_squared < 1e-10 || turn < -1e-6
                || (abs(turn) <= 1e-6 && delta_length_squared > edge_length_squared) {
                next_index = candidate;
            }
        }
        let closing = points[next_index] - points[start];
        if dot(closing, closing) < 1e-10 { break; }
        current = next_index;
    }
    if count < 3u || support_polygon_area(hull, count) < 1e-4 { return obb; }

    var expanded: array<vec2<f32>, 4>;
    // The small guard is geometric, not a color threshold: strict containment
    // checks below keep round-off from shaving the requested padded footprint.
    let guarded_padding = padding + 0.01;
    for (var index = 0u; index < count; index += 1u) {
        let previous = hull[(index + count - 1u) % count];
        let point = hull[index];
        let next_point = hull[(index + 1u) % count];
        let incoming = point - previous;
        let outgoing = next_point - point;
        let incoming_length = length(incoming);
        let outgoing_length = length(outgoing);
        if incoming_length < 1e-5 || outgoing_length < 1e-5 { return obb; }
        let first_normal = vec2(incoming.y, -incoming.x) / incoming_length;
        let second_normal = vec2(outgoing.y, -outgoing.x) / outgoing_length;
        let denominator = 1.0 + dot(first_normal, second_normal);
        if denominator < 0.04 { return obb; }
        let miter = (first_normal + second_normal) * (guarded_padding / denominator);
        if dot(miter, miter) > guarded_padding * guarded_padding * 64.0 { return obb; }
        let corner = point + miter;
        if !all(abs(corner) < vec2(1e18)) { return obb; }
        expanded[index] = corner;
    }

    // Every input endpoint must lie at least `padding` inside EVERY expanded
    // support edge. Convex combinations of those endpoints, including the whole
    // bilinear swept patch, then retain the same finite-width padding. Reject
    // numerically unstable or reversed polygons instead of cropping coverage.
    for (var index = 0u; index < count; index += 1u) {
        let a = expanded[index];
        let edge = expanded[(index + 1u) % count] - a;
        let edge_length = length(edge);
        if edge_length < 1e-5 { return obb; }
        for (var point_index = 0u; point_index < 4u; point_index += 1u) {
            if !(support_cross(edge, points[point_index] - a) >= padding * edge_length) {
                return obb;
            }
        }
    }
    let area = support_polygon_area(expanded, count);
    if !(area > 1e-5 && area < obb_area * 0.995) { return obb; }
    if count == 3u { expanded[3] = expanded[2]; }
    return expanded;
}
