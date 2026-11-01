//! Throwaway native A/B viewer. No saved application state is read or written.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Context;
use vello::{
    kurbo::Size,
    util::{RenderContext, RenderSurface},
    wgpu,
};
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalSize, PhysicalSize},
    event::{ElementState, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{KeyCode, PhysicalKey},
    window::{Window, WindowId},
};

use super::{
    background::GpuBackground,
    fixtures::{self, ModelFixture, MotionCase, Output},
    gpu::{FrameParams, GpuRibbons},
};
use crate::visualizer::{geometry::Pose, ModelComposite, Renderer, RibbonInstance};

const FRAME_INTERVAL: Duration = Duration::from_nanos(1_000_000_000 / 60);
const REFERENCE_SAMPLES: usize = 8;
const ACCUMULATION_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Current,
    Gpu,
    Reference,
    Coverage,
    Original,
}

impl Mode {
    fn label(self) -> &'static str {
        match self {
            Self::Current => "1: current endpoint ribbons",
            Self::Gpu => "2: optimized GPU prepared ribbons",
            Self::Reference => "3: sampled temporal reference (paused)",
            Self::Coverage => "4: GPU inverse-coverage ribbons",
            Self::Original => "5: original GPU ribbon prototype",
        }
    }

    fn is_gpu(self) -> bool {
        matches!(self, Self::Gpu | Self::Coverage | Self::Original)
    }
}

/// Continuous, bounded motion for the viewer. All three comparison paths sample
/// this same trajectory; frozen benchmark cases keep their original endpoints.
fn live_pose(case: MotionCase, frame: f32, height: u32) -> Pose {
    case.pose_at(-2.0 + 2.0 * (frame * 0.18).sin(), height)
}

struct Background {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
}

impl Background {
    fn new(
        context: &RenderContext,
        surface: &RenderSurface<'_>,
        composite: ModelComposite,
    ) -> Self {
        let handle = &context.devices[surface.dev_id];
        let texture = handle.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("PROTOTYPE retained neutral background"),
            size: wgpu::Extent3d {
                width: surface.config.width,
                height: surface.config.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: ACCUMULATION_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let mut encoder = handle.device.create_command_encoder(&Default::default());
        {
            let shade = match composite {
                ModelComposite::Additive => 0.02,
                ModelComposite::Multiplicative => 0.94,
            };
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("PROTOTYPE initialize background"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: shade,
                            g: shade,
                            b: shade,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
        }
        handle.queue.submit([encoder.finish()]);
        Self {
            _texture: texture,
            view,
        }
    }
}

struct Resources {
    window: Arc<Window>,
    context: RenderContext,
    surface: RenderSurface<'static>,
    background: Background,
    gpu_background: GpuBackground,
    _accumulation: wgpu::Texture,
    accumulation_view: wgpu::TextureView,
    current: Renderer,
    gpu: GpuRibbons,
    latest_slot: u32,
    sampled_frame: Option<f32>,
}

impl Resources {
    fn new(
        event_loop: &ActiveEventLoop,
        model: &ModelFixture,
        composite: ModelComposite,
    ) -> anyhow::Result<Self> {
        let window = Arc::new(
            event_loop.create_window(
                Window::default_attributes()
                    .with_title("Benga GPU ribbon PROTOTYPE")
                    .with_inner_size(LogicalSize::new(1200.0, 760.0))
                    .with_min_inner_size(LogicalSize::new(640.0, 400.0)),
            )?,
        );
        let mut context = RenderContext::new();
        let raw_surface = context.instance.create_surface(window.clone())?;
        pollster::block_on(crate::render::prepare_device(
            &mut context,
            Some(&raw_surface),
            wgpu::DeviceDescriptor::default(),
        ))?;
        let size = window.inner_size();
        let surface = pollster::block_on(context.create_render_surface(
            raw_surface,
            size.width.max(1),
            size.height.max(1),
            wgpu::PresentMode::AutoVsync,
        ))?;
        let device = &context.devices[surface.dev_id].device;
        let current = Renderer::new(device, ACCUMULATION_FORMAT);
        let gpu = GpuRibbons::new(device, ACCUMULATION_FORMAT, &model.vertices, &model.edges);
        let background = Background::new(&context, &surface, composite);
        let gpu_background = GpuBackground::new(device, ACCUMULATION_FORMAT, &background.view);
        let (accumulation, accumulation_view) = Self::accumulation(&context, &surface);
        Ok(Self {
            window,
            context,
            surface,
            background,
            gpu_background,
            _accumulation: accumulation,
            accumulation_view,
            current,
            gpu,
            latest_slot: 2,
            sampled_frame: None,
        })
    }

    fn accumulation(
        context: &RenderContext,
        surface: &RenderSurface<'_>,
    ) -> (wgpu::Texture, wgpu::TextureView) {
        let texture =
            context.devices[surface.dev_id]
                .device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some("PROTOTYPE full-resolution float accumulation"),
                    size: wgpu::Extent3d {
                        width: surface.config.width,
                        height: surface.config.height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: ACCUMULATION_FORMAT,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                });
        let view = texture.create_view(&Default::default());
        (texture, view)
    }

    fn resize(&mut self, size: PhysicalSize<u32>, composite: ModelComposite) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.context
            .resize_surface(&mut self.surface, size.width, size.height);
        self.background = Background::new(&self.context, &self.surface, composite);
        self.gpu_background = GpuBackground::new(
            &self.context.devices[self.surface.dev_id].device,
            ACCUMULATION_FORMAT,
            &self.background.view,
        );
        let (texture, view) = Self::accumulation(&self.context, &self.surface);
        self._accumulation = texture;
        self.accumulation_view = view;
        self.sampled_frame = None;
    }

    fn params(&self, case: MotionCase, frame: f32, slot: u32, alpha: f32) -> FrameParams {
        let width = self.surface.config.width;
        let height = self.surface.config.height;
        let mut params =
            FrameParams::new(width, height, live_pose(case, frame, height), slot, alpha);
        params.style[0] = fixtures::line_width(width, height);
        params.tint = [fixtures::RGB[0], fixtures::RGB[1], fixtures::RGB[2], 1.0];
        params
    }

    /// Each submission captures its own uniform update before the next update.
    /// Historical samples never travel back to the CPU.
    fn project(&mut self, case: MotionCase, frame: f32, slot: u32, alpha: f32) {
        let handle = &self.context.devices[self.surface.dev_id];
        self.gpu
            .update(&handle.queue, self.params(case, frame, slot, alpha));
        let mut encoder = handle.device.create_command_encoder(&Default::default());
        self.gpu.encode_project(&mut encoder, None);
        handle.queue.submit([encoder.finish()]);
    }

    fn update_history(&mut self, case: MotionCase, frame: f32, alpha: f32) {
        if self.sampled_frame == Some(frame) {
            let handle = &self.context.devices[self.surface.dev_id];
            self.gpu.update(
                &handle.queue,
                self.params(case, frame, self.latest_slot, alpha),
            );
            return;
        }
        if self.sampled_frame != Some(frame - 1.0) {
            for slot in 0..3 {
                self.project(case, frame - 2.0 + slot as f32, slot, alpha);
            }
            self.latest_slot = 2;
        } else {
            self.latest_slot = (self.latest_slot + 1) % 3;
            self.project(case, frame, self.latest_slot, alpha);
        }
        self.sampled_frame = Some(frame);
    }
}

struct Viewer {
    model: ModelFixture,
    resources: Option<Resources>,
    mode: Mode,
    motion_index: usize,
    frame: f32,
    alpha: f32,
    composite: ModelComposite,
    paused: bool,
    next_frame: Instant,
    animate_next: bool,
    cached_output: Option<Output>,
    output_dirty: bool,
    reference_samples: usize,
    failure: Option<anyhow::Error>,
    last_title: Instant,
    exit_at: Option<Instant>,
    presented_frames: u64,
}

impl Viewer {
    fn invalidate(&mut self, history: bool) {
        self.output_dirty = true;
        if let Some(resources) = &mut self.resources {
            if history {
                resources.sampled_frame = None;
            }
            resources.window.request_redraw();
        }
        self.last_title = Instant::now() - Duration::from_secs(1);
    }

    fn render(&mut self) -> anyhow::Result<()> {
        if self.animate_next && !self.paused {
            self.frame += 1.0;
            self.output_dirty = true;
        }
        self.animate_next = false;
        let resources = self.resources.as_mut().context("Prototype window exists")?;
        let size = resources.window.inner_size();
        if size.width == 0 || size.height == 0 {
            return Ok(());
        }
        let frame = resources.surface.surface.get_current_texture();
        let reconfigure = matches!(frame, wgpu::CurrentSurfaceTexture::Suboptimal(_));
        let texture = match frame {
            wgpu::CurrentSurfaceTexture::Success(texture)
            | wgpu::CurrentSurfaceTexture::Suboptimal(texture) => texture,
            wgpu::CurrentSurfaceTexture::Outdated => {
                resources.context.configure_surface(&resources.surface);
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(());
            }
            state => anyhow::bail!("Prototype surface acquisition failed: {state:?}"),
        };
        let before = Instant::now();
        let case = MotionCase::ALL[self.motion_index];
        resources.update_history(case, self.frame, self.alpha);
        if !self.mode.is_gpu() && (self.cached_output.is_none() || self.output_dirty) {
            let width = resources.surface.config.width;
            let height = resources.surface.config.height;
            if self.mode == Mode::Current {
                self.model.build_baseline_from_poses(
                    width,
                    height,
                    live_pose(case, self.frame - 2.0, height),
                    live_pose(case, self.frame, height),
                    self.alpha,
                    self.cached_output.get_or_insert_with(Output::default),
                );
            } else {
                self.cached_output = Some(self.model.reference_from_pose_fn(
                    width,
                    height,
                    self.alpha,
                    self.reference_samples,
                    |time| live_pose(case, self.frame - 2.0 + 2.0 * time, height),
                ));
            }
            self.output_dirty = false;
        }
        let handle = &resources.context.devices[resources.surface.dev_id];
        let view = texture.texture.create_view(&Default::default());
        let mut encoder = handle.device.create_command_encoder(&Default::default());
        if self.mode == Mode::Gpu {
            resources.gpu.encode_prepare(&mut encoder, None);
        }
        if self.mode.is_gpu() {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("PROTOTYPE retained background and GPU ribbons"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &resources.accumulation_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            resources.gpu_background.draw(&mut pass);
            match self.mode {
                Mode::Gpu => resources.gpu.draw_prepared(&mut pass, self.composite),
                Mode::Original => resources.gpu.draw_original(&mut pass, self.composite),
                Mode::Coverage => resources.gpu.draw(&mut pass, self.composite),
                _ => unreachable!(),
            }
        } else {
            let frame = self
                .cached_output
                .as_ref()
                .expect("CPU comparison geometry was prepared")
                .frame(self.composite);
            resources.current.draw(
                handle,
                &mut encoder,
                &resources.background.view,
                &resources.accumulation_view,
                Size::new(size.width as f64, size.height as f64),
                frame,
                None,
                None,
                1.0,
                None,
            );
        }
        resources.surface.blitter.copy(
            &handle.device,
            &mut encoder,
            &resources.accumulation_view,
            &view,
        );
        handle.queue.submit([encoder.finish()]);
        let cpu_ms = before.elapsed().as_secs_f64() * 1000.0;
        resources.window.pre_present_notify();
        handle.queue.present(texture);
        self.presented_frames += 1;
        if reconfigure {
            resources.context.configure_surface(&resources.surface);
        }
        if self.last_title.elapsed() >= Duration::from_millis(300) {
            let paused = if self.paused {
                "paused"
            } else {
                "60 Hz timeline"
            };
            let composite = match self.composite {
                ModelComposite::Additive => "add",
                ModelComposite::Multiplicative => "multiply",
            };
            resources.window.set_title(&format!(
                "PROTOTYPE | {} | {} | {} / {} edges | frame {:.0}, {paused} | {composite}, opacity {:.3} | CPU {:.2} ms | reference {} samples | 1/2/3/4/5, arrows, space, B, +/-",
                self.mode.label(), case.name(), self.model.id, self.model.edges.len(),
                self.frame, self.alpha, cpu_ms, self.reference_samples,
            ));
            self.last_title = Instant::now();
        }
        Ok(())
    }

    fn key(&mut self, event_loop: &ActiveEventLoop, code: KeyCode) {
        match code {
            KeyCode::Escape => event_loop.exit(),
            KeyCode::Digit1 => self.mode = Mode::Current,
            KeyCode::Digit2 => self.mode = Mode::Gpu,
            KeyCode::Digit4 => self.mode = Mode::Coverage,
            KeyCode::Digit5 => self.mode = Mode::Original,
            KeyCode::Digit3 => {
                self.mode = Mode::Reference;
                self.paused = true;
            }
            KeyCode::Space => {
                // The dense CPU reference is intentionally a paused comparison.
                if self.mode == Mode::Reference {
                    self.mode = Mode::Gpu;
                }
                self.paused = !self.paused;
                self.next_frame = Instant::now() + FRAME_INTERVAL;
            }
            KeyCode::ArrowRight | KeyCode::ArrowDown => {
                self.motion_index = (self.motion_index + 1) % MotionCase::ALL.len();
                self.frame = 0.0;
                self.invalidate(true);
                return;
            }
            KeyCode::ArrowLeft | KeyCode::ArrowUp => {
                self.motion_index =
                    (self.motion_index + MotionCase::ALL.len() - 1) % MotionCase::ALL.len();
                self.frame = 0.0;
                self.invalidate(true);
                return;
            }
            KeyCode::Period if self.paused => {
                self.frame += 1.0;
            }
            KeyCode::Comma if self.paused => {
                self.frame -= 1.0;
                self.invalidate(true);
                return;
            }
            KeyCode::KeyR => {
                self.frame = 0.0;
                self.invalidate(true);
                return;
            }
            KeyCode::KeyB => {
                self.composite = match self.composite {
                    ModelComposite::Additive => ModelComposite::Multiplicative,
                    ModelComposite::Multiplicative => ModelComposite::Additive,
                };
                if let Some(resources) = &mut self.resources {
                    resources.background =
                        Background::new(&resources.context, &resources.surface, self.composite);
                    resources.gpu_background = GpuBackground::new(
                        &resources.context.devices[resources.surface.dev_id].device,
                        ACCUMULATION_FORMAT,
                        &resources.background.view,
                    );
                }
            }
            KeyCode::Equal | KeyCode::NumpadAdd => {
                self.alpha = (self.alpha * 1.25).min(1.0);
            }
            KeyCode::Minus | KeyCode::NumpadSubtract => {
                self.alpha = (self.alpha / 1.25).max(0.0001);
            }
            _ => return,
        }
        self.invalidate(false);
    }
}

impl ApplicationHandler for Viewer {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.resources.is_some() {
            return;
        }
        match Resources::new(event_loop, &self.model, self.composite) {
            Ok(resources) => {
                let device = &resources.context.devices[resources.surface.dev_id].device;
                let limits = device.limits();
                let maximum =
                    u64::from(limits.max_storage_buffer_binding_size).min(limits.max_buffer_size);
                // The existing CPU renderer rounds capacity to a power of two.
                while self.reference_samples > 1
                    && (self.model.edges.len() * 2 * self.reference_samples).next_power_of_two()
                        as u64
                        * std::mem::size_of::<RibbonInstance>() as u64
                        > maximum
                {
                    self.reference_samples /= 2;
                }
                resources.window.request_redraw();
                self.resources = Some(resources);
            }
            Err(error) => {
                self.failure = Some(error);
                event_loop.exit();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(resources) = &mut self.resources {
                    resources.resize(size, self.composite);
                }
                self.invalidate(true);
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed && !event.repeat =>
            {
                if let PhysicalKey::Code(code) = event.physical_key {
                    self.key(event_loop, code);
                }
            }
            WindowEvent::RedrawRequested => {
                if let Err(error) = self.render() {
                    self.failure = Some(error);
                    event_loop.exit();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self
            .exit_at
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            event_loop.exit();
            return;
        }
        if self.paused {
            if let Some(deadline) = self.exit_at {
                event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
            } else {
                event_loop.set_control_flow(ControlFlow::Wait);
            }
            return;
        }
        let now = Instant::now();
        if now >= self.next_frame {
            self.animate_next = true;
            if let Some(resources) = &self.resources {
                resources.window.request_redraw();
            }
            self.next_frame = now + FRAME_INTERVAL;
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
    }
}

pub(super) fn run(model: ModelFixture, live_seconds: Option<f64>) -> anyhow::Result<()> {
    anyhow::ensure!(
        live_seconds.is_none_or(|seconds| seconds.is_finite() && seconds > 0.0),
        "--live-seconds must be finite and positive"
    );
    eprintln!("GPU ribbon PROTOTYPE: 1=current, 2=optimized GPU prepared ribbons, 3=paused sampled reference, 4=GPU inverse coverage, 5=original GPU prototype; arrows=motion, space=pause, comma/period=step, R=reset, B=blend, +/-=opacity, Esc=close. CPU title timings are not GPU timings. Reference sampling is bounded by the device's buffer limit.");
    let event_loop = EventLoop::new()?;
    let alpha = if model.edges.len() <= 2 { 0.9 } else { 0.04 };
    let mut viewer = Viewer {
        model,
        resources: None,
        mode: Mode::Gpu,
        motion_index: 2,
        frame: 0.0,
        alpha,
        composite: ModelComposite::Additive,
        paused: false,
        next_frame: Instant::now() + FRAME_INTERVAL,
        animate_next: false,
        cached_output: None,
        output_dirty: true,
        reference_samples: REFERENCE_SAMPLES,
        failure: None,
        last_title: Instant::now() - Duration::from_secs(1),
        exit_at: live_seconds.map(|seconds| Instant::now() + Duration::from_secs_f64(seconds)),
        presented_frames: 0,
    };
    event_loop.run_app(&mut viewer)?;
    if let Some(error) = viewer.failure {
        return Err(error);
    }
    if live_seconds.is_some() {
        anyhow::ensure!(
            viewer.presented_frames > 0,
            "native smoke run presented no frames"
        );
        eprintln!(
            "Native smoke run presented {} frames",
            viewer.presented_frames
        );
    }
    Ok(())
}
