struct Out { @builtin(position) position:vec4<f32>, @location(0) color:vec3<f32> }
@vertex fn vs(@location(0) clip:vec4<f32>, @location(1) color:vec3<f32>)->Out { var o:Out; o.position=clip; o.color=color; return o; }
@fragment fn fs(i:Out)->@location(0) vec4<f32> { return vec4<f32>(i.color,1.0); }
