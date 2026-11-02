struct BodyParams {
    rows: array<vec4<f32>, 3>,
    camera: vec4<f32>,
    depth: vec4<f32>,
    slide: vec4<f32>,
    viewport: vec4<f32>,
}
@group(0) @binding(0) var<uniform> params: BodyParams;

@vertex
fn body_vertex(@location(0) source: vec4<f32>) -> @builtin(position) vec4<f32> {
    let world = vec4(source.xyz + params.slide.xyz * source.w, 1.0);
    let view = vec3(dot(params.rows[0], world), dot(params.rows[1], world), dot(params.rows[2], world));
    let depth = view.z + params.depth.x;
    // Keep perspective homogeneous so triangles crossing the near plane are
    // clipped by the rasterizer instead of disappearing or projecting behind it.
    let center = vec2(2.0 * params.camera.x / params.viewport.x - 1.0,
                      1.0 - 2.0 * params.camera.y / params.viewport.y);
    let offset = 2.0 * view.xy * params.camera.z / params.viewport.xy;
    return vec4(center * depth + offset, depth - params.camera.w, depth);
}

@fragment
fn body_coverage() -> @location(0) vec4<f32> {
    return vec4(params.viewport.z, 0.0, 0.0, 1.0);
}

// These bindings are used only by the composition entry points, whose group0
// layout differs from the geometry entry points above.
@group(0) @binding(1) var coverage: texture_2d<f32>;
@group(1) @binding(0) var background: texture_2d<f32>;

@vertex
fn composite_vertex(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let xy = vec2(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4(xy * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn composite_body(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(position.xy);
    let opacity = textureLoad(coverage, pixel, 0).r;
    if opacity <= 0.0 { discard; }
    let color = textureLoad(background, pixel, 0);
    let alpha = color.a * opacity;
    return vec4(color.rgb * alpha, alpha);
}
