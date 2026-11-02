@group(0) @binding(0) var model: texture_2d<f32>;

struct Vertex {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex fn upsample_vertex(@builtin(vertex_index) index: u32) -> Vertex {
    let corners = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    let p = corners[index];
    return Vertex(vec4(p, 0.0, 1.0), vec2(p.x + 1.0, 1.0 - p.y) * 0.5);
}

@fragment fn upsample_fragment(v: Vertex) -> @location(0) vec4<f32> {
    // Choose one source texel so reduced model pixels keep hard boundaries.
    let size = vec2<i32>(textureDimensions(model));
    let texel = clamp(vec2<i32>(floor(v.uv * vec2<f32>(size))), vec2(0), size - 1);
    return textureLoad(model, texel, 0);
}
