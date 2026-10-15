use crate::document::{Editor, Object, RenderChange};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
#[cfg(not(target_os = "macos"))]
use std::sync::Arc;
use std::sync::OnceLock;
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
    instance_cache: IncrementalInstances,
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
            instance_cache: IncrementalInstances::default(),
        }
    }
    fn update(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, editor: &Editor, aspect: f32) {
        let camera = camera_matrix(editor, aspect);
        if self.previous_camera != Some(camera) {
            queue.write_buffer(&self.camera, 0, bytemuck::bytes_of(&camera));
            self.previous_camera = Some(camera);
        }
        let update = self.instance_cache.update(editor);
        for (index, batch) in self.batches.iter_mut().enumerate() {
            let instances = &self.instance_cache.groups[index];
            batch.instance_count = instances.len() as u32;
            if update.full
                || std::mem::size_of_val(instances.as_slice()) as u64 > batch.instances.capacity
            {
                batch.instances.upload(device, queue, instances);
            } else {
                batch
                    .instances
                    .previous
                    .resize(instances.len(), bytemuck::Zeroable::zeroed());
                for range in &update.ranges[index] {
                    let bytes = bytemuck::cast_slice(&instances[range.clone()]);
                    queue.write_buffer(
                        &batch.instances.buffer,
                        (range.start * std::mem::size_of::<Instance>()) as u64,
                        bytes,
                    );
                    batch.instances.previous[range.clone()]
                        .copy_from_slice(&instances[range.clone()]);
                }
            }
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
    #[cfg(not(target_os = "macos"))]
    pub async fn new(window: Arc<tauri::Window>) -> Result<Self, String> {
        let size = window.inner_size().map_err(|e| e.to_string())?;
        let instance = wgpu::Instance::default();
        let surface = instance.create_surface(window).map_err(|e| e.to_string())?;
        Self::from_surface(instance, surface, size.width, size.height).await
    }
    #[cfg(target_os = "macos")]
    pub async unsafe fn from_metal_layer(
        layer: *mut std::ffi::c_void,
        width: u32,
        height: u32,
    ) -> Result<Self, String> {
        let instance = wgpu::Instance::default();
        // Caller owns a live CAMetalLayer on the main thread. Metal's Surface retains it.
        let surface = unsafe {
            instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::CoreAnimationLayer(layer))
        }
        .map_err(|e| e.to_string())?;
        Self::from_surface(instance, surface, width, height).await
    }
    async fn from_surface(
        instance: wgpu::Instance,
        surface: wgpu::Surface<'static>,
        width: u32,
        height: u32,
    ) -> Result<Self, String> {
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
            .get_default_config(&adapter, width.max(1), height.max(1))
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
fn world_point(matrix: Matrix, point: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|i| matrix[3][i] + (0..3).map(|j| matrix[j][i] * point[j]).sum::<f32>())
}
fn object_bounds(object: &Object) -> ([f32; 3], [f32; 3]) {
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for mirrored in [false, true]
        .into_iter()
        .take(if object.mirror { 2 } else { 1 })
    {
        let matrix = model_matrix(object, mirrored);
        for point in geometry(&object.kind).iter().flatten() {
            let point = world_point(matrix, *point);
            for i in 0..3 {
                min[i] = min[i].min(point[i]);
                max[i] = max[i].max(point[i]);
            }
        }
    }
    (min, max)
}
pub fn frame_selection(editor: &mut Editor, aspect: f32) -> Result<(), String> {
    if !aspect.is_finite() || !(0.01..=100.).contains(&aspect) {
        return Err("Invalid viewport aspect".into());
    }
    if let Some(object) = editor
        .document
        .selected
        .and_then(|id| editor.indexed_object(id).map(|(_, o)| o))
    {
        let (min, max) = object_bounds(object);
        let target = std::array::from_fn(|i| (min[i] + max[i]) * 0.5);
        let radius = (dot(sub(max, min), sub(max, min)).sqrt() * 0.5).max(0.01);
        let half_fov = (aspect.min(1.) / 1.8).atan();
        editor.camera_target = target;
        editor.distance = (radius / half_fov.sin() * 1.15).clamp(0.05, 50000.);
    }
    Ok(())
}
pub fn pick(editor: &Editor, x: f32, y: f32, aspect: f32) -> Result<Option<u64>, String> {
    if ![x, y, aspect].iter().all(|v| v.is_finite())
        || x.abs() > 1.
        || y.abs() > 1.
        || !(0.01..=100.).contains(&aspect)
    {
        return Err("Invalid pick coordinates".into());
    }
    let (eye, right, up, forward) = camera_basis(editor);
    let ray = normalize(std::array::from_fn(|i| {
        forward[i] + right[i] * x * aspect / 1.8 + up[i] * y / 1.8
    }));
    let mut closest = f32::INFINITY;
    let mut hit = None;
    // On click only. Geometry stays cached in Rust; broad-phase spatial acceleration is a later step.
    for object in editor.document.objects.iter().filter(|o| o.visible) {
        for mirrored in [false, true]
            .into_iter()
            .take(if object.mirror { 2 } else { 1 })
        {
            let center = [
                if mirrored {
                    -object.position[0]
                } else {
                    object.position[0]
                },
                object.position[1],
                object.position[2],
            ];
            let radius = object.scale.iter().map(|v| v.abs()).fold(0., f32::max)
                * match object.kind.as_str() {
                    "sphere" => 0.5,
                    "plane" => 1.5,
                    _ => 0.867,
                };
            let to_center = sub(center, eye);
            let projection = dot(to_center, ray);
            if projection + radius < 0.
                || dot(to_center, to_center) - projection * projection > radius * radius
            {
                continue;
            }
            let matrix = model_matrix(object, mirrored);
            for face in geometry(&object.kind) {
                let [a, b, c] = face.map(|p| world_point(matrix, p));
                let ab = sub(b, a);
                let ac = sub(c, a);
                let h = cross(ray, ac);
                let det = dot(ab, h);
                if det.abs() < 1e-8 {
                    continue;
                }
                let inv = 1. / det;
                let s = sub(eye, a);
                let u = dot(s, h) * inv;
                if !(0. ..=1.).contains(&u) {
                    continue;
                }
                let q = cross(s, ab);
                let v = dot(ray, q) * inv;
                if v < 0. || u + v > 1. {
                    continue;
                }
                let t = dot(ac, q) * inv;
                let depth = t * dot(ray, forward);
                if depth >= (editor.distance * 0.1).min(0.1)
                    && depth <= (editor.distance * 4.).max(100.)
                    && t < closest
                {
                    closest = t;
                    hit = Some(object.id);
                }
            }
        }
    }
    Ok(hit)
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
pub fn camera_basis(editor: &Editor) -> ([f32; 3], [f32; 3], [f32; 3], [f32; 3]) {
    let offset = [
        editor.distance * editor.pitch.cos() * editor.yaw.sin(),
        editor.distance * editor.pitch.sin(),
        editor.distance * editor.pitch.cos() * editor.yaw.cos(),
    ];
    let eye = std::array::from_fn(|i| editor.camera_target[i] + offset[i]);
    let forward = normalize(offset.map(|v| -v));
    let right = normalize(cross(forward, [0., 1., 0.]));
    let up = cross(right, forward);
    (eye, right, up, forward)
}
fn camera_matrix(editor: &Editor, aspect: f32) -> Matrix {
    let (eye, right, up, forward) = camera_basis(editor);
    let far = (editor.distance * 4.).max(100.);
    let near = (editor.distance * 0.1).min(0.1);
    let z_scale = far / (far - near);
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
        -dot(eye, forward) * z_scale - near * z_scale,
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
fn shape_batch(object: &Object) -> usize {
    match object.kind.as_str() {
        "sphere" => 1,
        "plane" => 2,
        _ => 0,
    }
}
#[derive(Default)]
struct IncrementalInstances {
    base: InstanceCache,
    groups: [Vec<Instance>; 3],
    // ID -> (shape batch, first instance, instance count).
    slots: HashMap<u64, (usize, usize, usize)>,
    owners: [Vec<u64>; 3],
    revision: Option<u64>,
}
struct InstanceUpdate {
    full: bool,
    ranges: [Vec<Range<usize>>; 3],
    visited: usize,
}
impl IncrementalInstances {
    fn repair_slots(&mut self, batch: usize, start: usize) {
        for index in start..self.owners[batch].len() {
            let id = self.owners[batch][index];
            if index == 0 || self.owners[batch][index - 1] != id {
                self.slots.get_mut(&id).unwrap().1 = index;
            }
        }
    }
    fn update(&mut self, editor: &Editor) -> InstanceUpdate {
        let mut update = InstanceUpdate {
            full: false,
            ranges: Default::default(),
            visited: 0,
        };
        if self.revision == Some(editor.render_revision) {
            return update;
        }
        let full = self.revision.is_none()
            || self.revision.is_some_and(|revision| {
                revision > editor.render_revision
                    || editor
                        .render_changes
                        .front()
                        .is_none_or(|(first, _)| revision + 1 < *first)
                    || editor
                        .render_changes
                        .iter()
                        .any(|(r, change)| *r > revision && matches!(change, RenderChange::Reset))
            });
        if full {
            self.groups = self.base.build(editor).0;
            self.slots.clear();
            self.owners = Default::default();
            let mut offsets = [0; 3];
            for object in &editor.document.objects {
                update.visited += 1;
                if !object.visible {
                    continue;
                }
                let batch = match object.kind.as_str() {
                    "sphere" => 1,
                    "plane" => 2,
                    _ => 0,
                };
                let count = 1 + usize::from(object.mirror);
                self.slots.insert(object.id, (batch, offsets[batch], count));
                self.owners[batch].extend(std::iter::repeat_n(object.id, count));
                offsets[batch] += count;
            }
            update.full = true;
        } else {
            let revision = self.revision.unwrap();
            let mut ids = HashSet::new();
            for (r, change) in &editor.render_changes {
                if *r > revision {
                    if let RenderChange::Objects(changed) = change {
                        ids.extend(changed);
                    }
                }
            }
            // Remove vanished/moved/resized slots first so ordering searches see only live IDs.
            let mut dirty: [Vec<Range<usize>>; 3] = Default::default();
            let mut structural_start: [Option<usize>; 3] = [None; 3];
            for &id in &ids {
                if let Some(&(batch, start, count)) = self.slots.get(id) {
                    let desired = editor.indexed_object(*id).filter(|(_, o)| o.visible);
                    let same = desired.is_some_and(|(index, o)| {
                        let left = start.checked_sub(1).map(|i| self.owners[batch][i]);
                        let right = self.owners[batch].get(start + count).copied();
                        let ordered = left.is_none_or(|id| {
                            editor.indexed_object(id).is_some_and(|(i, _)| i < index)
                        }) && right.is_none_or(|id| {
                            editor.indexed_object(id).is_some_and(|(i, _)| i > index)
                        });
                        shape_batch(o) == batch && 1 + usize::from(o.mirror) == count && ordered
                    });
                    if !same {
                        self.groups[batch].drain(start..start + count);
                        self.owners[batch].drain(start..start + count);
                        self.slots.remove(id);
                        self.repair_slots(batch, start);
                        structural_start[batch] =
                            Some(structural_start[batch].map_or(start, |old| old.min(start)));
                        dirty[batch].push(start..self.groups[batch].len());
                    }
                }
            }
            for id in ids {
                update.visited += 1;
                let Some((index, object)) = editor.indexed_object(*id) else {
                    self.base.transforms.remove(id);
                    continue;
                };
                if !object.visible {
                    continue;
                }
                let key = TransformKey::from(object);
                let cached = self.base.transforms.entry(*id).or_insert_with(|| {
                    (
                        key,
                        [model_matrix(object, false), model_matrix(object, true)],
                    )
                });
                if cached.0 != key {
                    *cached = (
                        key,
                        [model_matrix(object, false), model_matrix(object, true)],
                    );
                }
                let color = if editor.document.selected == Some(object.id) {
                    [0.65, 0.79, 0.45, 1.]
                } else {
                    [0.58, 0.62, 0.67, 1.]
                };
                let count = 1 + usize::from(object.mirror);
                let values: Vec<Instance> = (0..count)
                    .map(|mirror| Instance {
                        model: cached.1[mirror],
                        color,
                    })
                    .collect();
                if let Some(&(batch, start, _)) = self.slots.get(id) {
                    for (offset, value) in values.into_iter().enumerate() {
                        if self.groups[batch][start + offset] != value {
                            self.groups[batch][start + offset] = value;
                            dirty[batch].push(start + offset..start + offset + 1);
                        }
                    }
                } else {
                    let batch = shape_batch(object);
                    let start = self.owners[batch]
                        .partition_point(|owner| editor.indexed_object(*owner).unwrap().0 < index);
                    self.groups[batch].splice(start..start, values);
                    self.owners[batch].splice(start..start, std::iter::repeat_n(*id, count));
                    self.slots.insert(*id, (batch, start, count));
                    self.repair_slots(batch, start);
                    structural_start[batch] =
                        Some(structural_start[batch].map_or(start, |old| old.min(start)));
                    dirty[batch].push(start..self.groups[batch].len());
                }
            }
            for (batch, ranges) in dirty.iter_mut().enumerate() {
                if let Some(start) = structural_start[batch] {
                    ranges.push(start..self.groups[batch].len());
                }
                // Structural edits can extend the tail after an earlier removal/insertion.
                // All slots after the earliest structural offset are conservatively dirty.
                ranges.sort_unstable_by_key(|range| range.start);
                for range in ranges.iter() {
                    let range = range.start.min(self.groups[batch].len())
                        ..range.end.min(self.groups[batch].len());
                    if range.is_empty() {
                        continue;
                    }
                    if let Some(last) = update.ranges[batch]
                        .last_mut()
                        .filter(|last| last.end >= range.start)
                    {
                        last.end = last.end.max(range.end);
                    } else {
                        update.ranges[batch].push(range);
                    }
                }
            }
        }
        self.revision = Some(editor.render_revision);
        update
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
    fn revisions_limit_visits_to_changed_ids_for_ten_thousand_objects() {
        use crate::editor::Command;
        let mut e = Editor::new();
        for id in 2..=10_000 {
            let mut o = e.document.objects[0].clone();
            o.id = id;
            e.document.objects.push(o);
        }
        e.invalidate_document();
        let mut cache = IncrementalInstances::default();
        assert!(cache.update(&e).full);
        e.execute(Command::Camera {
            yaw: 1.,
            pitch: 0.5,
            distance: 7.,
        })
        .unwrap();
        e.execute(Command::SetFrame { frame: 42 }).unwrap();
        assert_eq!(cache.update(&e).visited, 0);
        let mut object = e.document.objects[7000].clone();
        object.position[0] = 3.;
        e.execute(Command::Update { object }).unwrap();
        let result = cache.update(&e);
        assert!(!result.full);
        assert_eq!(result.visited, 1);
        assert_eq!(result.ranges[0], vec![7000..7001]);
        e.execute(Command::Select { id: 2 }).unwrap();
        let result = cache.update(&e);
        assert_eq!(result.visited, 2);
        assert_eq!(result.ranges[0], vec![0..2]);
        let mut object = e.document.objects[7000].clone();
        object.name = "New name".into();
        e.execute(Command::Update { object }).unwrap();
        let result = cache.update(&e);
        assert_eq!(result.visited, 1);
        assert!(result.ranges.iter().all(Vec::is_empty));
        assert!(cache.groups == scene_instances(&e));
        e.execute(Command::Undo).unwrap();
        cache.update(&e);
        assert!(cache.groups == scene_instances(&e));
        e.execute(Command::Undo).unwrap();
        cache.update(&e);
        assert!(cache.groups == scene_instances(&e));
        e.execute(Command::Redo).unwrap();
        cache.update(&e);
        assert!(cache.groups == scene_instances(&e));
        // Independent renderers consume the same retained history, never drain it.
        let mut second = IncrementalInstances::default();
        second.update(&e);
        let mut object = e.document.objects[7000].clone();
        object.position[1] = 2.;
        e.execute(Command::Update { object }).unwrap();
        assert_eq!(cache.update(&e).visited, 1);
        assert_eq!(second.update(&e).visited, 1);
        assert!(cache.groups == second.groups);
    }
    #[test]
    fn structural_changes_history_gaps_and_replacements_rebuild_safely() {
        use crate::editor::Command;
        let mut e = Editor::new();
        let mut cache = IncrementalInstances::default();
        cache.update(&e);
        for command in [
            Command::Add {
                kind: "sphere".into(),
            },
            Command::Delete,
            Command::Undo,
            Command::Redo,
        ] {
            e.execute(command).unwrap();
            assert!(!cache.update(&e).full);
            assert!(cache.groups == scene_instances(&e));
        }
        for flag in 0..3 {
            let mut object = e.document.objects[0].clone();
            match flag {
                0 => object.visible = false,
                1 => object.visible = true,
                _ => object.mirror = true,
            };
            e.execute(Command::Update { object }).unwrap();
            assert!(!cache.update(&e).full);
            assert!(cache.groups == scene_instances(&e));
        }
        let mut object = e.document.objects[0].clone();
        object.kind = "plane".into();
        e.execute(Command::Update { object }).unwrap();
        assert!(!cache.update(&e).full);
        for step in 1..=140 {
            let mut object = e.document.objects[0].clone();
            object.position[0] = step as f32;
            e.execute(Command::Update { object }).unwrap();
        }
        assert_eq!(e.render_changes.len(), 128);
        assert!(cache.update(&e).full);
        assert!(cache.groups == scene_instances(&e));
        e.document = crate::document::Document::default();
        e.invalidate_document();
        assert!(cache.update(&e).full);
        assert!(cache.groups == scene_instances(&e));
    }

    #[test]
    fn mixed_structural_edits_match_full_build_and_replay_gpu_ranges() {
        use crate::editor::Command;
        let mut e = Editor::new();
        let mut cache = IncrementalInstances::default();
        cache.update(&e);
        let mut shadow = cache.groups.clone();
        let mut seed = 42_u64;
        for step in 0..600 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let choice = (seed >> 32) as usize;
            let index = choice % e.document.objects.len().max(1);
            if e.document.objects.is_empty() || choice.is_multiple_of(8) {
                e.execute(Command::Add {
                    kind: ["cube", "sphere", "plane"][choice % 3].into(),
                })
                .unwrap();
            } else {
                let mut object = e.document.objects[index].clone();
                match choice % 8 {
                    1 => {
                        object.kind = ["cube", "sphere", "plane"][(choice / 8) % 3].into();
                        e.execute(Command::Update { object }).unwrap();
                    }
                    2 => {
                        object.visible = !object.visible;
                        e.execute(Command::Update { object }).unwrap();
                    }
                    3 => {
                        object.mirror = !object.mirror;
                        e.execute(Command::Update { object }).unwrap();
                    }
                    4 => {
                        e.execute(Command::Select { id: object.id }).unwrap();
                        e.execute(Command::Delete).unwrap();
                    }
                    5 => e.execute(Command::Undo).unwrap(),
                    6 => e.execute(Command::Redo).unwrap(),
                    _ => {
                        object.position = [step as f32, 1., 2.];
                        e.execute(Command::Update { object }).unwrap();
                    }
                }
            }
            // Accumulate several changes between render calls, including delete/re-add.
            if step % 7 != 0 {
                continue;
            }
            let update = cache.update(&e);
            assert!(!update.full);
            assert!(cache.groups == scene_instances(&e), "oracle step {step}");
            for (batch, values) in shadow.iter_mut().enumerate() {
                values.resize(cache.groups[batch].len(), bytemuck::Zeroable::zeroed());
                for range in &update.ranges[batch] {
                    values[range.clone()].copy_from_slice(&cache.groups[batch][range.clone()]);
                }
            }
            assert!(shadow == cache.groups, "range replay step {step}");
            e.document.validate().unwrap();
        }
        // Arbitrary same-length reorder via the core splice API also preserves batch order.
        let mut reversed = e.document.objects.clone();
        reversed.reverse();
        e.edit_span(0, reversed.len(), reversed, e.document.selected)
            .unwrap();
        cache.update(&e);
        assert!(cache.groups == scene_instances(&e));
    }
    #[test]
    fn structural_change_touches_only_source_and_destination_batches() {
        use crate::editor::Command;
        let mut e = Editor::new();
        for id in 2..=10_000 {
            let mut object = e.document.objects[0].clone();
            object.id = id;
            e.document.objects.push(object);
        }
        e.invalidate_document();
        e.select(None);
        let mut cache = IncrementalInstances::default();
        cache.update(&e);
        let mut object = e.document.objects[5000].clone();
        object.kind = "sphere".into();
        e.execute(Command::Update { object }).unwrap();
        let update = cache.update(&e);
        assert!(!update.full);
        assert_eq!(update.visited, 1);
        assert_eq!(update.ranges[0], vec![5000..9999]);
        assert_eq!(update.ranges[1], vec![0..1]);
        assert!(update.ranges[2].is_empty());
        assert!(cache.groups == scene_instances(&e));
        e.execute(Command::Undo).unwrap();
        cache.update(&e);
        assert!(cache.groups == scene_instances(&e));
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
                let mut object = e.document.objects[0].clone();
                object.position[0] += 0.1;
                e.execute(crate::editor::Command::Update { object })
                    .unwrap();
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
            for step in 0..18 {
                let mut object = e.document.objects[step % e.document.objects.len()].clone();
                object.mirror = !object.mirror;
                object.visible = step % 4 != 0;
                object.kind = ["cube", "sphere", "plane"][step % 3].into();
                e.execute(crate::editor::Command::Update { object })
                    .unwrap();
                scene.update(&device, &queue, &e, 1.);
                assert!(scene.instance_cache.groups == scene_instances(&e));
            }
            e.execute(crate::editor::Command::Add {
                kind: "cube".into(),
            })
            .unwrap();
            scene.update(&device, &queue, &e, 1.);
            e.execute(crate::editor::Command::Delete).unwrap();
            scene.update(&device, &queue, &e, 1.);
            e.execute(crate::editor::Command::Undo).unwrap();
            scene.update(&device, &queue, &e, 1.);
            for batch in 0..3 {
                if scene.instance_cache.groups[batch].is_empty() {
                    continue;
                }
                let expected =
                    bytemuck::cast_slice::<Instance, u8>(&scene.instance_cache.groups[batch]);
                let readback = device.create_buffer(&wgpu::BufferDescriptor {
                    label: None,
                    size: expected.len() as u64,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                });
                let mut encoder = device.create_command_encoder(&Default::default());
                encoder.copy_buffer_to_buffer(
                    &scene.batches[batch].instances.buffer,
                    0,
                    &readback,
                    0,
                    expected.len() as u64,
                );
                queue.submit([encoder.finish()]);
                let (tx, rx) = std::sync::mpsc::channel();
                readback
                    .slice(..)
                    .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
                device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                rx.recv().unwrap().unwrap();
                assert_eq!(&*readback.slice(..).get_mapped_range(), expected);
                readback.unmap();
            }
            e.document.objects.clear();
            e.invalidate_document();
            scene.update(&device, &queue, &e, 1.);
            assert!(scene.batches.iter().all(|b| b.instance_count == 0));
            assert!(device.pop_error_scope().await.is_none());
        });
    }
}

#[cfg(test)]
mod interaction_tests {
    use super::*;
    #[test]
    fn picking_follows_pan_projection_visibility_and_mirrored_geometry() {
        let mut e = Editor::new();
        e.yaw = 0.;
        e.pitch = 0.;
        assert_eq!(pick(&e, 0., 0., 1.).unwrap(), Some(1));
        assert_eq!(pick(&e, 0.99, 0.99, 1.).unwrap(), None);
        let mut object = e.document.objects[0].clone();
        object.position = [2., 0., 0.];
        object.mirror = true;
        e.execute(crate::editor::Command::Update {
            object: object.clone(),
        })
        .unwrap();
        e.camera_target = [-2., 0., 0.];
        assert_eq!(pick(&e, 0., 0., 1.).unwrap(), Some(1));
        object.visible = false;
        e.execute(crate::editor::Command::Update { object })
            .unwrap();
        assert_eq!(pick(&e, 0., 0., 1.).unwrap(), None);
        assert!(pick(&e, f32::NAN, 0., 1.).is_err());
    }
    #[test]
    fn picking_chooses_nearest_surface_and_handles_negative_scale() {
        let mut e = Editor::new();
        e.yaw = 0.;
        e.pitch = 0.;
        let mut object = e.document.objects[0].clone();
        object.id = 2;
        object.position[2] = 2.;
        object.scale = [-1., 2., 1.];
        let mut doc = e.document.clone();
        doc.objects.push(object);
        e.commit(doc).unwrap();
        assert_eq!(pick(&e, 0., 0., 1.).unwrap(), Some(2));
    }
    #[test]
    fn frame_selection_fits_rotated_mirrored_bounds_at_narrow_aspect() {
        let mut e = Editor::new();
        let mut object = e.document.objects[0].clone();
        object.position = [5., 2., 3.];
        object.rotation = [25., 40., 10.];
        object.scale = [2., 3., 1.];
        object.mirror = true;
        e.execute(crate::editor::Command::Update {
            object: object.clone(),
        })
        .unwrap();
        let history = e.undo.len();
        frame_selection(&mut e, 0.5).unwrap();
        assert_eq!(e.camera_target[0], 0.);
        let matrix = camera_matrix(&e, 0.5);
        for mirrored in [false, true] {
            for point in geometry(&object.kind).iter().flatten() {
                let world = world_point(model_matrix(&object, mirrored), *point);
                let clip: [f32; 4] = std::array::from_fn(|i| {
                    matrix[3][i] + (0..3).map(|j| matrix[j][i] * world[j]).sum::<f32>()
                });
                assert!(
                    clip[3] > 0.
                        && (clip[0] / clip[3]).abs() < 1.
                        && (clip[1] / clip[3]).abs() < 1.
                );
            }
        }
        assert_eq!(history, e.undo.len());
    }
}
