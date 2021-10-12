mod plugins;
mod terrain;

use crate::terrain::TerrainPlugin;
use crevice::std140::AsStd140;

use crate::plugins::{FlyCam, NoCameraPlayerPlugin};
use bevy::{
    app::App,
    asset::{Assets, Handle},
    core::Time,
    ecs::{
        entity::Entity,
        query::With,
        system::{Commands, Query, Res, ResMut},
    },
    log::*,
    math::Vec3,
    pbr2::{AmbientLight, PbrBundle, StandardMaterial},
    render2::{
        camera::PerspectiveCameraBundle,
        color::Color,
        mesh::{Indices, Mesh},
        render_resource::{MapMode, PrimitiveTopology, *},
        renderer::{RenderDevice, RenderQueue},
        shader::Shader,
    },
    tasks::{AsyncComputeTaskPool, Task},
    transform::components::Transform,
    window::WindowDescriptor,
    PipelinedDefaultPlugins,
};
use bytemuck::{Pod, Zeroable};
use noise::{NoiseFn, Perlin, Seedable};

use futures_lite::future;

use bevy_inspector_egui::WorldInspectorPlugin;

pub struct WorldMesh;

#[repr(C)]
#[derive(Debug, AsStd140, Copy, Clone, Zeroable, Pod)]
pub struct Triangle {
    pub a: Vec3,
    pub b: Vec3,
    pub c: Vec3,
}

#[repr(C)]
#[derive(Debug, AsStd140, Copy, Clone, Zeroable, Pod)]
pub struct Cube {
    pub triangle_count: u32,
    pub triangles: [Triangle; 5],
}

fn main() {
    let perlin = Perlin::new();

    perlin.set_seed(5225);

    let read_buffer: Option<Buffer> = None;

    App::new()
        .insert_resource(WindowDescriptor {
            width: 1920.0,
            height: 1080.0,
            title: "Lulw".to_string(),
            vsync: true,
            ..Default::default()
        })
        .insert_resource(LogSettings {
            level: Level::ERROR,
            ..Default::default()
        })
        .insert_resource(perlin)
        .insert_resource(read_buffer)
        .add_plugins(PipelinedDefaultPlugins)
        .add_plugin(WorldInspectorPlugin::new())
        .add_plugin(NoCameraPlayerPlugin)
        .add_plugin(TerrainPlugin)
        // .add_startup_system(gpu_setup)
        // .add_system(gpu_update)
        .run();
}

fn gpu_setup(
    mut commands: Commands,
    render_device: Res<RenderDevice>,
    render_queue: Res<RenderQueue>,
    thread_pool: Res<AsyncComputeTaskPool>,
    mut read_buffer_option: ResMut<Option<Buffer>>,
) {
    let number_of_cells = 64;

    let output_buffer_size =
        number_of_cells * number_of_cells * number_of_cells * (Cube::std140_size_static());
    let shader = Shader::from_wgsl(include_str!("../assets/shader.wgsl"));
    let shader_module = render_device.create_shader_module(&shader);

    let buffer = render_device.create_buffer(&BufferDescriptor {
        label: None,
        usage: BufferUsages::STORAGE | BufferUsages::COPY_SRC,
        mapped_at_creation: false,
        size: output_buffer_size as BufferAddress,
    });

    let bind_group_layout = render_device.create_bind_group_layout(&BindGroupLayoutDescriptor {
        label: None,
        entries: &[BindGroupLayoutEntry {
            binding: 0,
            visibility: ShaderStages::COMPUTE,
            ty: BindingType::Buffer {
                ty: BufferBindingType::Storage { read_only: false },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });

    let bind_group = render_device.create_bind_group(&BindGroupDescriptor {
        label: None,
        layout: &bind_group_layout,
        entries: &[BindGroupEntry {
            binding: 0,
            resource: buffer.as_entire_binding(),
        }],
    });

    let pipeline_layout = render_device.create_pipeline_layout(&PipelineLayoutDescriptor {
        label: None,
        push_constant_ranges: &[],
        bind_group_layouts: &[&bind_group_layout],
    });

    let compute_pipeline = render_device.create_compute_pipeline(&ComputePipelineDescriptor {
        label: None,
        layout: Some(&pipeline_layout),
        module: &shader_module,
        entry_point: "main",
    });

    let mut command_encoder =
        render_device.create_command_encoder(&CommandEncoderDescriptor { label: None });

    {
        let mut compute_pass =
            command_encoder.begin_compute_pass(&ComputePassDescriptor { label: None });
        compute_pass.set_pipeline(&compute_pipeline);
        compute_pass.set_bind_group(0, &*bind_group, &[]);

        compute_pass.dispatch(
            number_of_cells as u32 / 8,
            number_of_cells as u32 / 8,
            number_of_cells as u32 / 8,
        );
    }

    let read_buffer = render_device.create_buffer(&BufferDescriptor {
        label: None,
        usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ,
        mapped_at_creation: false,
        size: output_buffer_size as BufferAddress,
    });

    command_encoder.copy_buffer_to_buffer(
        &buffer,
        0,
        &read_buffer,
        0,
        output_buffer_size as BufferAddress,
    );

    let gpu_commands = command_encoder.finish();

    render_queue.submit([gpu_commands]);

    let buffer_slice = read_buffer.slice(..);

    let buffer_future = buffer_slice.map_async(MapMode::Read);

    let task = thread_pool.spawn(buffer_future);

    commands.spawn().insert(task);

    *read_buffer_option = Some(read_buffer);

    commands.insert_resource(AmbientLight {
        color: Color::WHITE,
        brightness: 1.0,
    });

    commands
        .spawn_bundle(PerspectiveCameraBundle {
            transform: Transform::from_xyz(-40.0, 40.0, 40.0).looking_at(Vec3::ZERO, Vec3::Y),
            ..Default::default()
        })
        .insert(FlyCam);
}

fn gpu_update(
    mut commands: Commands,
    mut compute_tasks: Query<(Entity, &mut Task<Result<(), BufferAsyncError>>)>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    read_buffer: Res<Option<Buffer>>,
) {
    for (entity, mut task) in compute_tasks.iter_mut() {
        if let Some(_) = future::block_on(future::poll_once(&mut *task)) {
            if let Some(buffer) = &*read_buffer {
                let buffer_slice = buffer.slice(..);

                let data = buffer_slice.get_mapped_range();

                let cubes: &[Std140Cube] = bytemuck::cast_slice(&data);

                let mut triangles: Vec<Triangle> = Vec::new();

                for cube in cubes.iter() {
                    let cube = Cube::from_std140(*cube);

                    for i in 0..cube.triangle_count {
                        triangles.push(cube.triangles[i as usize]);
                    }
                }

                let mut mesh = Mesh::new(PrimitiveTopology::TriangleList);

                let vertices = triangles
                    .iter()
                    .map(|triangle| [triangle.a, triangle.b, triangle.c])
                    .flatten()
                    .map(|vector| [vector.x, vector.y, vector.z])
                    .collect::<Vec<_>>();
                let indices = (0..vertices.len())
                    .map(|index| index as u32)
                    .collect::<Vec<u32>>();
                let uvs = (0..vertices.len())
                    .map(|_| [0.0, 0.0])
                    .collect::<Vec<[f32; 2]>>();

                let mut normals: Vec<[f32; 3]> = Vec::new();

                for triangle in indices.chunks(3) {
                    let a = Vec3::from(vertices[(triangle)[0] as usize]);
                    let b = Vec3::from(vertices[(triangle)[1] as usize]);
                    let c = Vec3::from(vertices[(triangle)[2] as usize]);

                    let normal = (b - a).cross(c - a).normalize();

                    normals.push(normal.into());
                    normals.push(normal.into());
                    normals.push(normal.into());
                }

                mesh.set_indices(Some(Indices::U32(indices)));

                mesh.set_attribute(Mesh::ATTRIBUTE_POSITION, vertices);
                mesh.set_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
                mesh.set_attribute(Mesh::ATTRIBUTE_UV_0, uvs);

                commands
                    .spawn_bundle(PbrBundle {
                        mesh: meshes.add(mesh),
                        material: materials.add(StandardMaterial {
                            base_color: Color::BLUE,
                            ..Default::default()
                        }),
                        transform: Transform::from_xyz(-32.0, -32.0, -32.0),
                        ..Default::default()
                    })
                    .insert(WorldMesh);

                drop(data);
                buffer.unmap();
            }

            commands
                .entity(entity)
                .remove::<Task<Result<(), BufferAsyncError>>>();
        }
    }
}
