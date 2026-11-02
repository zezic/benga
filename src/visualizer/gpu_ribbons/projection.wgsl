struct ProjectionParams {
    rows: array<vec4<f32>, 3>,
    camera: vec4<f32>,
    depth: vec4<f32>,
    slide: vec4<f32>,
    history: vec4<u32>,
}
@group(0) @binding(0) var<uniform> params: ProjectionParams;
@group(0) @binding(1) var<storage, read> vertices: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> history: array<vec4<f32>>;

@compute @workgroup_size(64)
fn project_vertices(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= params.history.x { return; }
    let source = vertices[id.x];
    let world = vec4(source.xyz + params.slide.xyz * source.w, 1.0);
    let view = vec3(dot(params.rows[0], world), dot(params.rows[1], world), dot(params.rows[2], world));
    let raw_depth = view.z + params.depth.x;
    let depth = max(raw_depth, params.camera.w);
    let xy = params.camera.xy + vec2(view.x, -view.y) * (params.camera.z / depth);
    var projected = vec4(0.0);
    // Comparison also rejects NaN, and overflow is never passed to edge setup.
    if raw_depth > params.camera.w && all(abs(xy) <= vec2(3.402823e38)) {
        let span = params.depth.z - params.depth.y;
        var opacity = 1.0;
        if span > 1e-6 {
            opacity = 1.0 - params.depth.w * clamp((depth - params.depth.y) / span, 0.0, 1.0);
        }
        projected = vec4(xy, opacity, 1.0);
    }
    if params.history.z != 0u {
        // A switch, seek or physical resize seeds all three poses in one pass.
        for (var index = 0u; index < 3u; index += 1u) {
            history[index * params.history.x + id.x] = projected;
        }
    } else {
        history[params.history.y * params.history.x + id.x] = projected;
    }
}
