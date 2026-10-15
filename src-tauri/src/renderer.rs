use crate::document::{Editor, Object};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::{Arc, OnceLock};
use wgpu::util::DeviceExt;

type Matrix = [[f32; 4]; 4];
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    position: [f32; 3],
    edge_a: [f32; 3],
    edge_b: [f32; 3],
}
#[repr(C)]
#[derive(Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct Instance {
    model: Matrix,
    color: [f32; 4],
}
struct Batch {
    vertices: wgpu::Buffer,
    vertex_count: u32,
    instances: VertexBuffer,
    instance_count: u32,
}
struct GpuScene {
    triangles: wgpu::RenderPipeline,
    lines: wgpu::RenderPipeline,
    camera: wgpu::Buffer,
    camera_bind: wgpu::BindGroup,
    previous_camera: Option<Matrix>,
    grid: wgpu::Buffer,
    batches: [Batch; 3],
    instance_cache: InstanceCache,
}
pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    depth: wgpu::TextureView,
    scene: GpuScene,
}
struct VertexBuffer {
    buffer: wgpu::Buffer,
    capacity: u64,
    previous: Vec<Instance>,
}
impl VertexBuffer {
    fn new(device: &wgpu::Device, capacity: u64) -> Self {
        let mut usage = wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST;
        if cfg!(test) {
            usage |= wgpu::BufferUsages::COPY_SRC;
        }
        Self {
            buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Reusable instances"),
                size: capacity,
                usage,
                mapped_at_creation: false,
            }),
            capacity,
            previous: Vec::new(),
        }
    }
    // Returns bytes submitted, also used by GPU regression tests.
    fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, values: &[Instance]) -> u64 {
        let required = std::mem::size_of_val(values) as u64;
        if required > self.capacity {
            // A fresh allocation has no retained contents: upload the entire active prefix.
            *self = Self::new(device, allocation_size(required));
        }
        let ranges = changed_ranges(&self.previous, values);
        let mut uploaded = 0;
        for range in ranges {
            let bytes = bytemuck::cast_slice(&values[range.clone()]);
            let offset = (range.start * std::mem::size_of::<Instance>()) as u64;
            queue.write_buffer(&self.buffer, offset, bytes);
            uploaded += bytes.len() as u64;
        }
        self.previous.clear();
        self.previous.extend_from_slice(values);
        uploaded
    }
}
// Consecutive dirty slots share one write. Compaction after deletion is handled
// by comparing the new active prefix; stale tail slots are excluded by draw count.
fn changed_ranges(previous: &[Instance], next: &[Instance]) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = None;
    for (index, value) in next.iter().enumerate() {
        if previous.get(index) != Some(value) {
            start.get_or_insert(index);
        } else if let Some(begin) = start.take() {
            ranges.push(begin..index);
        }
    }
    if let Some(begin) = start {
        ranges.push(begin..next.len());
    }
    ranges
}
fn allocation_size(required: u64) -> u64 {
    required.max(256).next_power_of_two()
}
fn depth_view(device: &wgpu::Device, width: u32, height: u32) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("Viewport depth"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
        .create_view(&Default::default())
}
impl GpuScene {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Instanced scene"),
            source: wgpu::ShaderSource::Wgsl(include_str!("scene.wgsl").into()),
        });
        let camera = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Camera matrix"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(64),
                },
                count: None,
            }],
        });
        let camera_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &bind_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera.as_entire_binding(),
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[&bind_layout],
            push_constant_ranges: &[],
        });
        let vertex_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0=>Float32x3,1=>Float32x3,2=>Float32x3],
        };
        let instance_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Instance>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &wgpu::vertex_attr_array![3=>Float32x4,4=>Float32x4,5=>Float32x4,6=>Float32x4,7=>Float32x4],
        };
        let make = |topology, entry, buffers: &[wgpu::VertexBufferLayout<'_>]| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(entry),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    buffers,
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs"),
                    compilation_options: Default::default(),
                    targets: &[Some(format.into())],
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
        let lines = make(
            wgpu::PrimitiveTopology::LineList,
            "vs_grid",
            std::slice::from_ref(&vertex_layout),
        );
        let triangles = make(
            wgpu::PrimitiveTopology::TriangleList,
            "vs_mesh",
            &[vertex_layout, instance_layout],
        );
        let static_buffer = |vertices: &[Vertex]| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Shared geometry"),
                contents: bytemuck::cast_slice(vertices),
                usage: wgpu::BufferUsages::VERTEX,
            })
        };
        let mut grid = Vec::new();
        for i in -20..=20 {
            let color = if i == 0 {
                [0.32, 0.4, 0.32]
            } else {
                [0.025, 0.032, 0.04]
            };
            for position in [
                [i as f32, -0.51, -20.],
                [i as f32, -0.51, 20.],
                [-20., -0.51, i as f32],
                [20., -0.51, i as f32],
            ] {
                grid.push(Vertex {
                    position,
                    edge_a: color,
                    edge_b: [0.; 3],
                });
            }
        }
        let batches = ["cube", "sphere", "plane"].map(|kind| {
            let vertices: Vec<Vertex> = geometry(kind)
                .iter()
                .flat_map(|face| {
                    face.map(|position| Vertex {
                        position,
                        edge_a: sub(face[1], face[0]),
                        edge_b: sub(face[2], face[0]),
                    })
                })
                .collect();
            Batch {
                vertices: static_buffer(&vertices),
                vertex_count: vertices.len() as u32,
                instances: VertexBuffer::new(device, 256),
                instance_count: 0,
            }
        });
        Self {
            triangles,
            lines,
            camera,
            camera_bind,
            previous_camera: None,
            grid: static_buffer(&grid),
            batches,
            instance_cache: InstanceCache::default(),
        }
    }
    fn update(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, editor: &Editor, aspect: f32) {
        let camera = camera_matrix(editor, aspect);
        if self.previous_camera != Some(camera) {
            queue.write_buffer(&self.camera, 0, bytemuck::bytes_of(&camera));
            self.previous_camera = Some(camera);
        }
        for (batch, instances) in self
            .batches
            .iter_mut()
            .zip(self.instance_cache.build(editor).0)
        {
            batch.instance_count = instances.len() as u32;
            batch.instances.upload(device, queue, &instances);
        }
    }
    fn encode(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        depth: &wgpu::TextureView,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Instanced viewport"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
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
                view: depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_bind_group(0, &self.camera_bind, &[]);
        pass.set_pipeline(&self.lines);
        pass.set_vertex_buffer(0, self.grid.slice(..));
        pass.draw(0..164, 0..1);
        pass.set_pipeline(&self.triangles);
        for batch in &self.batches {
            if batch.instance_count == 0 {
                continue;
            }
            pass.set_vertex_buffer(0, batch.vertices.slice(..));
            pass.set_vertex_buffer(1, batch.instances.buffer.slice(..));
            pass.draw(0..batch.vertex_count, 0..batch.instance_count);
        }
    }
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
        let scene = GpuScene::new(&device, config.format);
        let depth = depth_view(&device, config.width, config.height);
        Ok(Self {
            surface,
            device,
            queue,
            config,
            depth,
            scene,
        })
    }
    pub fn resize(&mut self, w: u32, h: u32) {
        if w > 0 && h > 0 && (w != self.config.width || h != self.config.height) {
            self.config.width = w;
            self.config.height = h;
            self.depth = depth_view(&self.device, w, h);
            self.surface.configure(&self.device, &self.config);
        }
    }
    pub fn draw(&mut self, editor: &Editor) -> Result<(), String> {
        let frame = match self.surface.get_current_texture() {
            Ok(frame) => frame,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                self.surface.configure(&self.device, &self.config);
                self.surface
                    .get_current_texture()
                    .map_err(|e| e.to_string())?
            }
            Err(wgpu::SurfaceError::Timeout) => return Ok(()),
            Err(err) => return Err(err.to_string()),
        };
        self.scene.update(
            &self.device,
            &self.queue,
            editor,
            self.config.width as f32 / self.config.height as f32,
        );
        let view = frame.texture.create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        self.scene.encode(&mut encoder, &view, &self.depth);
        self.queue.submit([encoder.finish()]);
        frame.present();
        Ok(())
    }
}
// Preserve the existing degree-based X, then Y, then Z rotation contract.
// ThreeX's Euler composition is not imported: its contract differs for compound rotations.
fn model_matrix(object: &Object, mirrored: bool) -> Matrix {
    let mut matrix = [[0.; 4]; 4];
    for (axis, column) in matrix.iter_mut().take(3).enumerate() {
        let mut p = [0.; 3];
        p[axis] = object.scale[axis];
        for axis in 0..3 {
            let a = (axis + 1) % 3;
            let b = (axis + 2) % 3;
            let (s, c) = object.rotation[axis].to_radians().sin_cos();
            let x = p[a] * c - p[b] * s;
            p[b] = p[a] * s + p[b] * c;
            p[a] = x;
        }
        if mirrored {
            p[0] = -p[0];
        }
        column[..3].copy_from_slice(&p);
    }
    matrix[3] = [
        if mirrored {
            -object.position[0]
        } else {
            object.position[0]
        },
        object.position[1],
        object.position[2],
        1.,
    ];
    matrix
}
fn camera_matrix(editor: &Editor, aspect: f32) -> Matrix {
    let eye = [
        editor.distance * editor.pitch.cos() * editor.yaw.sin(),
        editor.distance * editor.pitch.sin(),
        editor.distance * editor.pitch.cos() * editor.yaw.cos(),
    ];
    let forward = normalize(eye.map(|v| -v));
    let right = normalize(cross(forward, [0., 1., 0.]));
    let up = cross(right, forward);
    let z_scale = 100. / 99.9;
    let mut m = [[0.; 4]; 4];
    for (axis, column) in m.iter_mut().take(3).enumerate() {
        *column = [
            right[axis] * 1.8 / aspect,
            up[axis] * 1.8,
            forward[axis] * z_scale,
            forward[axis],
        ];
    }
    m[3] = [
        -dot(eye, right) * 1.8 / aspect,
        -dot(eye, up) * 1.8,
        -dot(eye, forward) * z_scale - 0.1001,
        -dot(eye, forward),
    ];
    m
}
#[derive(Clone, Copy, PartialEq)]
struct TransformKey {
    position: [f32; 3],
    rotation: [f32; 3],
    scale: [f32; 3],
}
impl From<&Object> for TransformKey {
    fn from(object: &Object) -> Self {
        Self {
            position: object.position,
            rotation: object.rotation,
            scale: object.scale,
        }
    }
}
#[derive(Default)]
struct InstanceCache {
    transforms: HashMap<u64, (TransformKey, [Matrix; 2])>,
}
impl InstanceCache {
    // No document revision assumption: safe for Undo, file replacement and reused IDs.
    fn build(&mut self, editor: &Editor) -> ([Vec<Instance>; 3], usize) {
        let ids: HashSet<u64> = editor.document.objects.iter().map(|o| o.id).collect();
        self.transforms.retain(|id, _| ids.contains(id));
        let mut rebuilt = 0;
        let mut groups: [Vec<Instance>; 3] = Default::default();
        for object in editor.document.objects.iter().filter(|o| o.visible) {
            let key = TransformKey::from(object);
            let cached = self.transforms.entry(object.id).or_insert_with(|| {
                rebuilt += 1;
                (
                    key,
                    [model_matrix(object, false), model_matrix(object, true)],
                )
            });
            if cached.0 != key {
                cached.0 = key;
                cached.1 = [model_matrix(object, false), model_matrix(object, true)];
                rebuilt += 1;
            }
            let index = match object.kind.as_str() {
                "sphere" => 1,
                "plane" => 2,
                _ => 0,
            };
            let color = if editor.document.selected == Some(object.id) {
                [0.65, 0.79, 0.45, 1.]
            } else {
                [0.58, 0.62, 0.67, 1.]
            };
            for mirrored in 0..=usize::from(object.mirror) {
                groups[index].push(Instance {
                    model: cached.1[mirrored],
                    color,
                });
            }
        }
        (groups, rebuilt)
    }
}
#[cfg(test)]
fn scene_instances(editor: &Editor) -> [Vec<Instance>; 3] {
    InstanceCache::default().build(editor).0
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
type Face = [[f32; 3]; 3];
fn geometry(kind: &str) -> &'static [Face] {
    static CUBE: OnceLock<Vec<Face>> = OnceLock::new();
    static SPHERE: OnceLock<Vec<Face>> = OnceLock::new();
    static PLANE: OnceLock<Vec<Face>> = OnceLock::new();
    let cache = match kind {
        "sphere" => &SPHERE,
        "plane" => &PLANE,
        _ => &CUBE,
    };
    cache.get_or_init(|| build_geometry(kind))
}
fn build_geometry(kind: &str) -> Vec<Face> {
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

#[cfg(test)]
mod tests {
    use super::*;
    fn transform(m: Matrix, p: [f32; 4]) -> [f32; 4] {
        std::array::from_fn(|row| (0..4).map(|col| m[col][row] * p[col]).sum())
    }
    fn close(a: [f32; 4], b: [f32; 4]) {
        for (a, b) in a.into_iter().zip(b) {
            assert!((a - b).abs() < 0.0001, "{a} != {b}");
        }
    }
    #[test]
    fn model_preserves_compound_rotation_negative_scale_and_world_mirror() {
        let mut o = Editor::new().document.objects[0].clone();
        o.position = [2., -1., 3.];
        o.rotation = [31., -47., 73.];
        o.scale = [-2., 0.5, 3.];
        for p in [[0., 0., 0.], [0.5, -0.5, 0.5], [-1., 2., -3.]] {
            let mut expected = p;
            for (v, scale) in expected.iter_mut().zip(o.scale) {
                *v *= scale;
            }
            // Reference the previous per-vertex path, independently of matrix construction.
            for axis in 0..3 {
                let a = (axis + 1) % 3;
                let b = (axis + 2) % 3;
                let (s, c) = o.rotation[axis].to_radians().sin_cos();
                let x = expected[a] * c - expected[b] * s;
                expected[b] = expected[a] * s + expected[b] * c;
                expected[a] = x;
            }
            for (v, t) in expected.iter_mut().zip(o.position) {
                *v += t;
            }
            close(
                transform(model_matrix(&o, false), [p[0], p[1], p[2], 1.]),
                [expected[0], expected[1], expected[2], 1.],
            );
            close(
                transform(model_matrix(&o, true), [p[0], p[1], p[2], 1.]),
                [-expected[0], expected[1], expected[2], 1.],
            );
        }
    }
    #[test]
    fn camera_preserves_projection_across_aspects() {
        let e = Editor::new();
        let eye = [
            e.distance * e.pitch.cos() * e.yaw.sin(),
            e.distance * e.pitch.sin(),
            e.distance * e.pitch.cos() * e.yaw.cos(),
        ];
        let f = normalize(eye.map(|v| -v));
        let r = normalize(cross(f, [0., 1., 0.]));
        let u = cross(r, f);
        for aspect in [0.5, 1., 2.] {
            for p in [[0., 0., 0.], [1., -2., 3.]] {
                let d = sub(p, eye);
                let z = dot(d, f);
                close(
                    transform(camera_matrix(&e, aspect), [p[0], p[1], p[2], 1.]),
                    [
                        dot(d, r) * 1.8 / aspect,
                        dot(d, u) * 1.8,
                        z * 100. / 99.9 - 0.1001,
                        z,
                    ],
                );
            }
        }
    }
    #[test]
    fn batches_track_selection_visibility_mirror_and_undo() {
        let mut e = Editor::new();
        let mut next = e.document.clone();
        next.objects[0].mirror = true;
        let mut sphere = next.objects[0].clone();
        sphere.id = 2;
        sphere.kind = "sphere".into();
        sphere.mirror = false;
        let mut plane = sphere.clone();
        plane.id = 3;
        plane.kind = "plane".into();
        plane.visible = false;
        next.objects.extend([sphere, plane]);
        e.commit(next).unwrap();
        let groups = scene_instances(&e);
        assert_eq!(groups.map(|g| g.len()), [2, 1, 0]);
        assert_eq!(scene_instances(&e)[0][0].color, [0.65, 0.79, 0.45, 1.]);
        e.undo();
        assert_eq!(scene_instances(&e).map(|g| g.len()), [1, 0, 0]);
        e.redo();
        assert_eq!(scene_instances(&e).map(|g| g.len()), [2, 1, 0]);
        e.document.objects.clear();
        assert_eq!(scene_instances(&e).map(|g| g.len()), [0, 0, 0]);
    }
    #[test]
    fn primitive_geometry_and_instance_layout() {
        assert_eq!(std::mem::size_of::<Instance>(), 80);
        assert_eq!(std::mem::size_of::<Vertex>(), 36);
        for (kind, count) in [("cube", 12), ("sphere", 768), ("plane", 2)] {
            assert_eq!(geometry(kind).len(), count);
            assert!(std::ptr::eq(geometry(kind), geometry(kind)));
            assert!(geometry(kind)
                .iter()
                .flatten()
                .flatten()
                .all(|v| v.is_finite()));
        }
        for n in [0, 256, 257, 1_000_000] {
            let size = allocation_size(n);
            assert!(size >= n && size.is_power_of_two());
        }
    }
    #[test]
    fn dirty_ranges_cover_edits_compaction_and_regrowth() {
        let base = scene_instances(&Editor::new())[0][0];
        let mut old = vec![base; 8];
        for (index, item) in old.iter_mut().enumerate() {
            item.model[3][0] = index as f32;
        }
        assert_eq!(changed_ranges(&[], &old), vec![0..8]);
        assert!(changed_ranges(&old, &old).is_empty());
        let mut edited = old.clone();
        for index in [1, 2, 5, 7] {
            edited[index].color[0] = 0.1;
        }
        assert_eq!(changed_ranges(&old, &edited), vec![1..3, 5..6, 7..8]);
        let mut simulated = old.clone();
        for range in changed_ranges(&old, &edited) {
            simulated[range.clone()].copy_from_slice(&edited[range]);
        }
        assert!(simulated == edited);
        let mut removed = old.clone();
        removed.remove(2);
        assert_eq!(changed_ranges(&old, &removed), vec![2..7]);
        assert!(changed_ranges(&old, &old[..4]).is_empty());
        assert_eq!(changed_ranges(&old[..4], &old), vec![4..8]);
        assert!(changed_ranges(&old, &[]).is_empty());
        assert_eq!(changed_ranges(&[], &old), vec![0..8]);
    }

    #[test]
    fn camera_only_and_single_object_updates_minimize_instance_writes() {
        let mut editor = Editor::new();
        for id in 2..=10_000 {
            let mut object = editor.document.objects[0].clone();
            object.id = id;
            editor.document.objects.push(object);
        }
        let old = scene_instances(&editor);
        editor.yaw += 0.3;
        editor.frame += 1;
        editor.document.objects[2].name = "Renamed".into();
        let same = scene_instances(&editor);
        assert!(changed_ranges(&old[0], &same[0]).is_empty());
        editor.document.objects[123].position[0] = 2.;
        let new = scene_instances(&editor);
        assert_eq!(changed_ranges(&old[0], &new[0]), vec![123..124]);
        editor.document.selected = Some(editor.document.objects[1].id);
        let selected = scene_instances(&editor);
        assert_eq!(changed_ranges(&new[0], &selected[0]), vec![0..2]);
        editor.document.objects[123].mirror = true;
        let mirrored = scene_instances(&editor);
        assert_eq!(mirrored[0].len(), 10_001);
        assert!(!changed_ranges(&selected[0], &mirrored[0]).is_empty());
    }

    #[test]
    fn matrix_cache_tracks_edits_undo_visibility_and_document_replacement() {
        use crate::editor::Command;
        let mut e = Editor::new();
        let mut cache = InstanceCache::default();
        assert_eq!(cache.build(&e).1, 1);
        e.yaw += 1.;
        e.frame += 1;
        e.document.objects[0].name = "Renamed".into();
        assert_eq!(cache.build(&e).1, 0);
        e.document.selected = None;
        e.document.objects[0].mirror = true;
        assert_eq!(cache.build(&e).1, 0);
        let mut object = e.document.objects[0].clone();
        object.position[0] = 3.;
        e.execute(Command::Update { object }).unwrap();
        assert_eq!(cache.build(&e).1, 1);
        assert_eq!(cache.build(&e).1, 0);
        e.execute(Command::Undo).unwrap();
        assert_eq!(cache.build(&e).1, 1);
        e.execute(Command::Redo).unwrap();
        assert_eq!(cache.build(&e).1, 1);
        e.document.objects[0].visible = false;
        e.document.objects[0].rotation = [15., 30., 45.];
        assert_eq!(cache.build(&e).1, 0);
        e.document.objects[0].visible = true;
        let (groups, rebuilt) = cache.build(&e);
        assert_eq!(rebuilt, 1);
        assert!(groups == scene_instances(&e));
        e.document.objects.clear();
        assert_eq!(cache.build(&e).1, 0);
        assert!(cache.transforms.is_empty());
        e.document = crate::document::Document::default();
        assert_eq!(cache.build(&e).1, 1);
        let mut replacement = e.document.clone();
        replacement.objects[0].scale = [2., -1., 3.];
        e.document = replacement;
        let (groups, rebuilt) = cache.build(&e);
        assert_eq!(rebuilt, 1);
        assert!(groups == scene_instances(&e));
    }

    #[test]
    #[ignore = "Requires a GPU adapter; run explicitly on a GPU-enabled host"]
    fn gpu_offscreen_instanced_scene() {
        pollster::block_on(async {
            let adapter = wgpu::Instance::default()
                .request_adapter(&Default::default())
                .await
                .expect("GPU adapter unavailable");
            let (device, queue) = adapter.request_device(&Default::default()).await.unwrap();
            device.push_error_scope(wgpu::ErrorFilter::Validation);
            let mut scene = GpuScene::new(&device, wgpu::TextureFormat::Rgba8Unorm);
            let base = scene_instances(&Editor::new())[0][0];
            let mut values = vec![base; 2];
            let mut buffer = VertexBuffer::new(&device, 256);
            assert_eq!(buffer.upload(&device, &queue, &values), 160);
            assert_eq!(buffer.upload(&device, &queue, &values), 0);
            values[1].model[3][0] = 3.;
            assert_eq!(buffer.upload(&device, &queue, &values), 80);
            values.push(base); // Within the retained allocation.
            assert_eq!(buffer.upload(&device, &queue, &values), 80);
            values.push(base); // Expansion must initialize even unchanged slots.
            assert_eq!(buffer.upload(&device, &queue, &values), 320);
            assert_eq!(buffer.upload(&device, &queue, &values[..1]), 0);
            assert_eq!(buffer.upload(&device, &queue, &values), 240);
            assert_eq!(buffer.upload(&device, &queue, &[]), 0);
            assert_eq!(buffer.upload(&device, &queue, &values), 320);
            // Read back actual GPU contents: validate offsets and retained unchanged slots.
            values[2].color[1] = 0.25;
            assert_eq!(buffer.upload(&device, &queue, &values), 80);
            let readback = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Instance delta verification"),
                size: 320,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let mut encoder = device.create_command_encoder(&Default::default());
            encoder.copy_buffer_to_buffer(&buffer.buffer, 0, &readback, 0, 320);
            queue.submit([encoder.finish()]);
            let (tx, rx) = std::sync::mpsc::channel();
            readback
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |result| {
                    tx.send(result).unwrap();
                });
            device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            rx.recv().unwrap().unwrap();
            {
                let actual = readback.slice(..).get_mapped_range();
                assert_eq!(&*actual, bytemuck::cast_slice::<Instance, u8>(&values));
            }
            readback.unmap();
            // Exercise growth, multiple kinds, mirror, resize, and empty-after-populated.
            let mut e = Editor::new();
            for id in 2..20 {
                let mut o = e.document.objects[0].clone();
                o.id = id;
                o.kind = ["cube", "sphere", "plane"][(id % 3) as usize].into();
                o.rotation = [31., -47., 73.];
                o.scale = [-1., 0.5, 2.];
                o.mirror = true;
                e.document.objects.push(o);
            }
            for size in [64, 128] {
                let target = device.create_texture(&wgpu::TextureDescriptor {
                    label: None,
                    size: wgpu::Extent3d {
                        width: size,
                        height: size,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                });
                let depth = depth_view(&device, size, size);
                scene.update(&device, &queue, &e, 1.);
                let mut encoder = device.create_command_encoder(&Default::default());
                scene.encode(
                    &mut encoder,
                    &target.create_view(&Default::default()),
                    &depth,
                );
                queue.submit([encoder.finish()]);
                device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            }
            e.document.objects.clear();
            scene.update(&device, &queue, &e, 1.);
            assert!(scene.batches.iter().all(|b| b.instance_count == 0));
            assert!(device.pop_error_scope().await.is_none());
        });
    }
}
