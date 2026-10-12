struct Camera { vp: mat4x4<f32> }
@group(0) @binding(0) var<uniform> camera: Camera;
struct Out {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec3<f32>,
}
@vertex fn vs_mesh(
    @location(0) p: vec3<f32>,
    @location(1) edge_a: vec3<f32>,
    @location(2) edge_b: vec3<f32>,
    @location(3) a: vec4<f32>,
    @location(4) b: vec4<f32>,
    @location(5) c: vec4<f32>,
    @location(6) d: vec4<f32>,
    @location(7) color: vec4<f32>,
) -> Out {
    let model = mat4x4<f32>(a,b,c,d);
    let edges = cross((model * vec4<f32>(edge_a,0.0)).xyz, (model * vec4<f32>(edge_b,0.0)).xyz);
    // Degenerate pole triangles have zero area; avoid normalize(0).
    let normal = edges / max(length(edges),0.00001);
    let light = 0.35 + 0.65 * abs(dot(normal,normalize(vec3<f32>(0.4,1.0,0.6))));
    var out: Out;
    out.position = camera.vp * model * vec4<f32>(p,1.0);
    out.color = color.rgb * light;
    return out;
}
@vertex fn vs_grid(@location(0) p: vec3<f32>, @location(1) color: vec3<f32>) -> Out {
    var out: Out;
    out.position = camera.vp * vec4<f32>(p,1.0);
    out.color = color;
    return out;
}
@fragment fn fs(input: Out) -> @location(0) vec4<f32> { return vec4<f32>(input.color,1.0); }
