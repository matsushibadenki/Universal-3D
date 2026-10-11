use crate::document::Editor;
use std::sync::Arc;
use wgpu::util::DeviceExt;
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    clip: [f32; 4],
    color: [f32; 3],
}
pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    triangles: wgpu::RenderPipeline,
    lines: wgpu::RenderPipeline,
}
impl Renderer {
    pub async fn new(window: Arc<tauri::Window>) -> Result<Self, String> {
        let size = window.inner_size().map_err(|e| e.to_string())?;
        let instance = wgpu::Instance::default();
        let surface = instance.create_surface(window).map_err(|e| e.to_string())?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await
            .map_err(|e| e.to_string())?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .map_err(|e| e.to_string())?;
        let config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .ok_or("No surface configuration")?;
        surface.configure(&device, &config);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Scene shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("scene.wgsl").into()),
        });
        let make = |topology| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("Viewport"),
                layout: None,
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<Vertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &wgpu::vertex_attr_array![0=>Float32x4,1=>Float32x3],
                    }],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs"),
                    compilation_options: Default::default(),
                    targets: &[Some(config.format.into())],
                }),
                primitive: wgpu::PrimitiveState {
                    topology,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: wgpu::TextureFormat::Depth32Float,
                    depth_write_enabled: true,
                    depth_compare: wgpu::CompareFunction::LessEqual,
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                multiview: None,
                cache: None,
            })
        };
        let triangles = make(wgpu::PrimitiveTopology::TriangleList);
        let lines = make(wgpu::PrimitiveTopology::LineList);
        Ok(Self {
            surface,
            device,
            queue,
            config,
            triangles,
            lines,
        })
    }
    pub fn resize(&mut self, w: u32, h: u32) {
        if w > 0 && h > 0 {
            self.config.width = w;
            self.config.height = h;
            self.surface.configure(&self.device, &self.config);
        }
    }
    pub fn draw(&mut self, e: &Editor) -> Result<(), String> {
        let frame = match self.surface.get_current_texture() {
            Ok(f) => f,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                self.surface.configure(&self.device, &self.config);
                self.surface
                    .get_current_texture()
                    .map_err(|e| e.to_string())?
            }
            Err(wgpu::SurfaceError::Timeout) => return Ok(()),
            Err(err) => return Err(err.to_string()),
        };
        let eye = [
            e.distance * e.pitch.cos() * e.yaw.sin(),
            e.distance * e.pitch.sin(),
            e.distance * e.pitch.cos() * e.yaw.cos(),
        ];
        let forward = normalize([-eye[0], -eye[1], -eye[2]]);
        let right = normalize(cross(forward, [0., 1., 0.]));
        let up = cross(right, forward);
        let project = |p: [f32; 3], color: [f32; 3]| {
            let d = [p[0] - eye[0], p[1] - eye[1], p[2] - eye[2]];
            let z = dot(d, forward);
            Vertex {
                clip: [
                    dot(d, right) * 1.8 / (self.config.width as f32 / self.config.height as f32),
                    dot(d, up) * 1.8,
                    z * 100. / 99.9 - 0.1001,
                    z,
                ],
                color,
            }
        };
        let mut grid = vec![];
        for i in -20..=20 {
            let c = if i == 0 {
                [0.32, 0.4, 0.32]
            } else {
                [0.025, 0.032, 0.04]
            };
            grid.extend([
                project([i as f32, -0.51, -20.], c),
                project([i as f32, -0.51, 20.], c),
                project([-20., -0.51, i as f32], c),
                project([20., -0.51, i as f32], c),
            ]);
        }
        let mut mesh = vec![];
        for o in e.document.objects.iter().filter(|o| o.visible) {
            let base = if e.document.selected == Some(o.id) {
                [0.65, 0.79, 0.45]
            } else {
                [0.58, 0.62, 0.67]
            };
            for mirrored in 0..=u8::from(o.mirror) {
                for face in geometry(&o.kind) {
                    let transformed = face.map(|mut p| {
                        for a in 0..3 {
                            p[a] *= o.scale[a];
                        }
                        for axis in 0..3 {
                            let a = (axis + 1) % 3;
                            let b = (axis + 2) % 3;
                            let r = o.rotation[axis].to_radians();
                            let (s, c) = r.sin_cos();
                            let x = p[a] * c - p[b] * s;
                            p[b] = p[a] * s + p[b] * c;
                            p[a] = x;
                        }
                        for a in 0..3 {
                            p[a] += o.position[a];
                        }
                        if mirrored == 1 {
                            p[0] = -p[0];
                        }
                        p
                    });
                    let normal = normalize(cross(
                        sub(transformed[1], transformed[0]),
                        sub(transformed[2], transformed[0]),
                    ));
                    let light = 0.35 + 0.65 * dot(normal, normalize([0.4, 1., 0.6])).abs();
                    let color = base.map(|v| v * light);
                    for p in transformed {
                        mesh.push(project(p, color));
                    }
                }
            }
        }
        let vb = |data: &[Vertex]| {
            self.device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: None,
                    contents: bytemuck::cast_slice(data),
                    usage: wgpu::BufferUsages::VERTEX,
                })
        };
        let gb = vb(&grid);
        let mb = if mesh.is_empty() {
            None
        } else {
            Some(vb(&mesh))
        };
        let depth = self
            .device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("Depth"),
                size: wgpu::Extent3d {
                    width: self.config.width,
                    height: self.config.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Depth32Float,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
            .create_view(&Default::default());
        let view = frame.texture.create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.008,
                            g: 0.01,
                            b: 0.013,
                            a: 1.,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.lines);
            pass.set_vertex_buffer(0, gb.slice(..));
            pass.draw(0..grid.len() as u32, 0..1);
            if let Some(b) = &mb {
                pass.set_pipeline(&self.triangles);
                pass.set_vertex_buffer(0, b.slice(..));
                pass.draw(0..mesh.len() as u32, 0..1);
            }
        }
        self.queue.submit([encoder.finish()]);
        frame.present();
        Ok(())
    }
}
fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn normalize(a: [f32; 3]) -> [f32; 3] {
    let l = dot(a, a).sqrt().max(0.00001);
    a.map(|v| v / l)
}
fn geometry(kind: &str) -> Vec<[[f32; 3]; 3]> {
    if kind == "plane" {
        return vec![
            [[-1., -0.5, -1.], [1., -0.5, -1.], [1., -0.5, 1.]],
            [[-1., -0.5, -1.], [1., -0.5, 1.], [-1., -0.5, 1.]],
        ];
    }
    if kind == "sphere" {
        let mut f = vec![];
        let point = |u: f32, v: f32| {
            let a = u * std::f32::consts::TAU;
            let b = v * std::f32::consts::PI;
            [
                0.5 * b.sin() * a.cos(),
                0.5 * b.cos(),
                0.5 * b.sin() * a.sin(),
            ]
        };
        for i in 0..24 {
            for j in 0..16 {
                let a = point(i as f32 / 24., j as f32 / 16.);
                let b = point((i + 1) as f32 / 24., j as f32 / 16.);
                let c = point((i + 1) as f32 / 24., (j + 1) as f32 / 16.);
                let d = point(i as f32 / 24., (j + 1) as f32 / 16.);
                f.extend([[a, b, c], [a, c, d]]);
            }
        }
        return f;
    }
    let p = [
        [-0.5, -0.5, -0.5],
        [0.5, -0.5, -0.5],
        [0.5, 0.5, -0.5],
        [-0.5, 0.5, -0.5],
        [-0.5, -0.5, 0.5],
        [0.5, -0.5, 0.5],
        [0.5, 0.5, 0.5],
        [-0.5, 0.5, 0.5],
    ];
    [
        [0, 1, 2, 3],
        [4, 7, 6, 5],
        [0, 4, 5, 1],
        [3, 2, 6, 7],
        [0, 3, 7, 4],
        [1, 5, 6, 2],
    ]
    .into_iter()
    .flat_map(|[a, b, c, d]| [[p[a], p[b], p[c]], [p[a], p[c], p[d]]])
    .collect()
}
