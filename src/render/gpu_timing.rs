//! Asynchronous timestamp spans for a frame's submitted GPU pipeline.
//! Four timestamp words cover total/model spans; drawing never waits for a sample.
use std::{
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    },
    time::Duration,
};

use vello::wgpu;

const SLOT_COUNT: usize = 6;
const QUERY_BYTES: u64 = 32;
const PENDING: u8 = 0;
const READY: u8 = 1;
const FAILED: u8 = 2;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GpuFrameTag {
    pub downsize: f32,
    pub viewport: [u32; 2],
    pub generation: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct GpuFrameTiming {
    pub tag: GpuFrameTag,
    pub elapsed: Duration,
    /// The actual model raster pass, excluding full-resolution UI/resolve work.
    pub model_elapsed: Duration,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Free,
    Recording,
    Encoded,
    Submitted,
    Mapping,
}

struct Slot {
    queries: wgpu::QuerySet,
    resolved: wgpu::Buffer,
    readback: wgpu::Buffer,
    completed: Arc<AtomicU8>,
    state: State,
    tag: Option<GpuFrameTag>,
    serial: u64,
}

pub(super) struct GpuFrameTimer {
    queue: wgpu::Queue,
    slots: Vec<Slot>,
    marker_pipeline: wgpu::ComputePipeline,
    marker_bindings: wgpu::BindGroup,
    _marker_buffer: wgpu::Buffer,
    timestamp_period_ns: f64,
    next_slot: usize,
    next_serial: u64,
    delivered_serial: u64,
}

impl GpuFrameTimer {
    pub(super) fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Option<Self> {
        let timestamp_period_ns = f64::from(queue.get_timestamp_period());
        if !device.features().contains(wgpu::Features::TIMESTAMP_QUERY)
            || !timestamp_period_ns.is_finite()
            || timestamp_period_ns <= 0.0
        {
            return None;
        }
        // Metal can omit timestamp writes for empty encoders. A real one-thread
        // storage write keeps the start marker alive without frame-sized work.
        let marker_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Frame timestamp marker counter"),
            size: 4,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let marker_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Frame timestamp start marker"),
            source: wgpu::ShaderSource::Wgsl(
                "@group(0) @binding(0) var<storage,read_write> marker:atomic<u32>;
                 @compute @workgroup_size(1) fn mark() { atomicAdd(&marker, 1u); }"
                    .into(),
            ),
        });
        let marker_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("Frame timestamp start marker"),
            layout: None,
            module: &marker_shader,
            entry_point: Some("mark"),
            compilation_options: Default::default(),
            cache: None,
        });
        let marker_bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Frame timestamp start marker"),
            layout: &marker_pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: marker_buffer.as_entire_binding(),
            }],
        });
        let slots = (0..SLOT_COUNT)
            .map(|_| Slot {
                queries: device.create_query_set(&wgpu::QuerySetDescriptor {
                    label: Some("Asynchronous frame and model timestamps"),
                    ty: wgpu::QueryType::Timestamp,
                    count: 4,
                }),
                resolved: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("Asynchronous frame timestamp resolve"),
                    size: wgpu::QUERY_RESOLVE_BUFFER_ALIGNMENT,
                    usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                }),
                readback: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("Asynchronous frame timestamp readback"),
                    size: QUERY_BYTES,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }),
                completed: Arc::new(AtomicU8::new(PENDING)),
                state: State::Free,
                tag: None,
                serial: 0,
            })
            .collect();
        Some(Self {
            queue: queue.clone(),
            slots,
            marker_pipeline,
            marker_bindings,
            _marker_buffer: marker_buffer,
            timestamp_period_ns,
            next_slot: 0,
            next_serial: 0,
            delivered_serial: 0,
        })
    }

    /// Check completed maps once without waiting. Late older completions never
    /// replace a more recent delivered frame, including when the ring wraps.
    pub(super) fn poll(&mut self, device: &wgpu::Device) -> Option<GpuFrameTiming> {
        let _ = device.poll(wgpu::PollType::Poll);
        let mut newest: Option<(u64, GpuFrameTiming)> = None;
        for slot in &mut self.slots {
            if slot.state != State::Mapping {
                continue;
            }
            let completed = slot.completed.load(Ordering::Acquire);
            if completed == PENDING {
                continue;
            }
            let elapsed = if completed == READY {
                slot.readback
                    .slice(..QUERY_BYTES)
                    .get_mapped_range()
                    .ok()
                    .and_then(|bytes| decode_elapsed(&bytes, self.timestamp_period_ns))
            } else {
                None
            };
            if completed == READY {
                // Every BufferView above has dropped before this unmap/reuse.
                slot.readback.unmap();
            } else {
                // A failed map has no mapped range to unmap. Recreate only
                // this tiny staging allocation on the exceptional path.
                slot.readback = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("Replacement frame timestamp readback"),
                    size: QUERY_BYTES,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
            }
            let tag = slot.tag.take();
            slot.state = State::Free;
            slot.completed.store(PENDING, Ordering::Release);
            if let (Some(tag), Some((elapsed, model_elapsed))) = (tag, elapsed) {
                if slot.serial > self.delivered_serial
                    && newest
                        .as_ref()
                        .is_none_or(|(serial, _)| slot.serial > *serial)
                {
                    newest = Some((
                        slot.serial,
                        GpuFrameTiming {
                            tag,
                            elapsed,
                            model_elapsed,
                        },
                    ));
                }
            }
        }
        // Metal counter samples must be resolved after their producing command
        // buffer has completed. Completion callbacks only signal readiness;
        // this poll submits the resolve/copy without ever waiting on the GPU.
        let ready: Vec<_> = self
            .slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| {
                slot.state == State::Submitted && slot.completed.load(Ordering::Acquire) == READY
            })
            .map(|(index, _)| index)
            .collect();
        if !ready.is_empty() {
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Resolve completed frame timestamps"),
            });
            for &index in &ready {
                let slot = &self.slots[index];
                encoder.resolve_query_set(&slot.queries, 0..4, &slot.resolved, 0);
                encoder.copy_buffer_to_buffer(&slot.resolved, 0, &slot.readback, 0, QUERY_BYTES);
            }
            self.queue.submit([encoder.finish()]);
            for index in ready {
                let slot = &mut self.slots[index];
                slot.completed.store(PENDING, Ordering::Release);
                slot.state = State::Mapping;
                let completed = Arc::clone(&slot.completed);
                slot.readback
                    .slice(..QUERY_BYTES)
                    .map_async(wgpu::MapMode::Read, move |result| {
                        completed.store(
                            if result.is_ok() { READY } else { FAILED },
                            Ordering::Release,
                        );
                    });
            }
        }
        newest.map(|(serial, timing)| {
            self.delivered_serial = serial;
            timing
        })
    }

    /// Submit the start marker before subsequent Vello/frame GPU submissions.
    /// A full ring skips telemetry instead of applying backpressure to drawing.
    pub(super) fn begin(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        tag: GpuFrameTag,
    ) -> Option<usize> {
        let index = (0..self.slots.len())
            .map(|offset| (self.next_slot + offset) % self.slots.len())
            .find(|index| self.slots[*index].state == State::Free)?;
        self.next_slot = (index + 1) % self.slots.len();
        let slot = &mut self.slots[index];
        self.next_serial = self.next_serial.saturating_add(1);
        slot.serial = self.next_serial;
        slot.tag = Some(tag);
        slot.state = State::Recording;
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Frame GPU timestamp start"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("Frame GPU timestamp start marker"),
                timestamp_writes: Some(wgpu::ComputePassTimestampWrites {
                    query_set: &slot.queries,
                    beginning_of_pass_write_index: Some(0),
                    end_of_pass_write_index: None,
                }),
            });
            pass.set_pipeline(&self.marker_pipeline);
            pass.set_bind_group(0, &self.marker_bindings, &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }
        queue.submit([encoder.finish()]);
        Some(index)
    }

    /// The model raster pass writes start/end to queries 1/2; the actual final
    /// frame pass writes its end to query 3. Empty encoders are unreliable here.
    pub(super) fn query_set(&self, index: usize) -> Option<&wgpu::QuerySet> {
        self.slots
            .get(index)
            .filter(|slot| slot.state == State::Recording)
            .map(|slot| &slot.queries)
    }

    /// Arm the sample after the actual final frame pass has written query 3.
    pub(super) fn finish(&mut self, _encoder: &mut wgpu::CommandEncoder, index: usize) {
        let Some(slot) = self
            .slots
            .get_mut(index)
            .filter(|slot| slot.state == State::Recording)
        else {
            return;
        };
        slot.state = State::Encoded;
    }

    /// Call only after the encoder from `finish` has been submitted.
    pub(super) fn submitted(&mut self, index: usize) {
        let Some(slot) = self
            .slots
            .get_mut(index)
            .filter(|slot| slot.state == State::Encoded)
        else {
            return;
        };
        slot.completed.store(PENDING, Ordering::Release);
        slot.state = State::Submitted;
        let completed = Arc::clone(&slot.completed);
        self.queue
            .on_submitted_work_done(move || completed.store(READY, Ordering::Release));
    }
}

fn decode_elapsed(bytes: &[u8], period_ns: f64) -> Option<(Duration, Duration)> {
    let begin = u64::from_ne_bytes(bytes.get(..8)?.try_into().ok()?);
    let model_begin = u64::from_ne_bytes(bytes.get(8..16)?.try_into().ok()?);
    let model_end = u64::from_ne_bytes(bytes.get(16..24)?.try_into().ok()?);
    let end = u64::from_ne_bytes(bytes.get(24..32)?.try_into().ok()?);
    if begin == 0
        || end <= begin
        || model_begin == 0
        || model_end <= model_begin
        || [begin, model_begin, model_end, end].contains(&u64::MAX)
        || !period_ns.is_finite()
        || period_ns <= 0.0
    {
        return None;
    }
    let duration = |ticks| {
        Duration::try_from_secs_f64(ticks as f64 * period_ns * 1e-9)
            .ok()
            .filter(|elapsed| !elapsed.is_zero())
    };
    Some((duration(end - begin)?, duration(model_end - model_begin)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamps(values: [u64; 4]) -> Vec<u8> {
        values.into_iter().flat_map(u64::to_ne_bytes).collect()
    }

    #[test]
    fn timestamp_decode_checks_written_monotonic_finite_values() {
        assert_eq!(
            decode_elapsed(&stamps([10, 14, 20, 30]), 2.5),
            Some((Duration::from_nanos(50), Duration::from_nanos(15)))
        );
        for values in [
            [0, 0, 0, 0],
            [0, 14, 20, 30],
            [10, 14, 14, 30],
            [10, 20, 14, 30],
            [30, 14, 20, 10],
            [10, 14, 20, u64::MAX],
        ] {
            assert!(decode_elapsed(&stamps(values), 1.0).is_none());
        }
        // Metal pipeline stages can overlap across encoders. Each interval
        // must be monotonic, but one need not contain the other.
        assert_eq!(
            decode_elapsed(&stamps([10, 5, 40, 30]), 1.0),
            Some((Duration::from_nanos(20), Duration::from_nanos(35)))
        );
        for period in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(decode_elapsed(&stamps([10, 14, 20, 30]), period).is_none());
        }
        assert!(decode_elapsed(&[0; 24], 1.0).is_none());
    }

    #[test]
    #[ignore = "requires GPU timestamp-query support; validates asynchronous markers and ring reuse"]
    fn asynchronous_frame_timestamps_preserve_tags_and_recycle_a_busy_ring() -> anyhow::Result<()> {
        use std::time::Instant;

        use anyhow::{ensure, Context};
        use vello::util::{DeviceHandle, RenderContext};

        let mut context = RenderContext::new();
        let id = pollster::block_on(crate::render::prepare_device(
            &mut context,
            None,
            wgpu::DeviceDescriptor {
                required_features: wgpu::Features::TIMESTAMP_QUERY,
                ..Default::default()
            },
        ))?;
        let handle = &context.devices[id];
        let mut timer = GpuFrameTimer::new(&handle.device, &handle.queue)
            .context("GPU timestamp queries are unavailable")?;
        let data = handle.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Frame timer regression workload"),
            size: 4096 * 4,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let shader = handle
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("Frame timer regression compute"),
                source: wgpu::ShaderSource::Wgsl(
                    "@group(0) @binding(0) var<storage,read_write> words:array<u32>;
                @compute @workgroup_size(64) fn work(@builtin(global_invocation_id) id:vec3<u32>) {
                    if id.x >= arrayLength(&words) { return; }
                    var value = words[id.x] + id.x;
                    for (var n=0u; n<64u; n+=1u) { value=value*1664525u+1013904223u; }
                    words[id.x]=value;
                }"
                    .into(),
                ),
            });
        let pipeline = handle
            .device
            .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("Frame timer regression workload"),
                layout: None,
                module: &shader,
                entry_point: Some("work"),
                compilation_options: Default::default(),
                cache: None,
            });
        let bindings = handle.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Frame timer regression workload"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: data.as_entire_binding(),
            }],
        });
        let submit = |timer: &mut GpuFrameTimer, tag: GpuFrameTag| -> anyhow::Result<usize> {
            let slot = timer
                .begin(&handle.device, &handle.queue, tag)
                .context("free timing slot")?;
            let mut encoder = handle.device.create_command_encoder(&Default::default());
            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("Frame timer regression measured work"),
                    timestamp_writes: Some(wgpu::ComputePassTimestampWrites {
                        query_set: timer.query_set(slot).context("active timestamp slot")?,
                        beginning_of_pass_write_index: Some(1),
                        end_of_pass_write_index: Some(2),
                    }),
                });
                pass.set_pipeline(&pipeline);
                pass.set_bind_group(0, &bindings, &[]);
                pass.dispatch_workgroups(64, 1, 1);
            }
            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("Frame timer regression final resolve equivalent"),
                    timestamp_writes: Some(wgpu::ComputePassTimestampWrites {
                        query_set: timer.query_set(slot).context("active timestamp slot")?,
                        beginning_of_pass_write_index: None,
                        end_of_pass_write_index: Some(3),
                    }),
                });
                pass.set_pipeline(&pipeline);
                pass.set_bind_group(0, &bindings, &[]);
                pass.dispatch_workgroups(1, 1, 1);
            }
            timer.finish(&mut encoder, slot);
            handle.queue.submit([encoder.finish()]);
            timer.submitted(slot);
            Ok(slot)
        };
        fn drain(
            timer: &mut GpuFrameTimer,
            handle: &DeviceHandle,
        ) -> anyhow::Result<GpuFrameTiming> {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut newest = None;
            loop {
                // Production uses this same non-waiting poll. Sleeping belongs
                // only to this bounded test harness, never the playback path.
                if let Some(sample) = timer.poll(&handle.device) {
                    ensure!(!sample.elapsed.is_zero() && sample.elapsed < Duration::from_secs(10));
                    ensure!(!sample.model_elapsed.is_zero());
                    newest = Some(sample);
                }
                if timer.slots.iter().all(|slot| slot.state == State::Free) {
                    break;
                }
                ensure!(
                    Instant::now() < deadline,
                    "timestamp mapping did not finish"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
            newest.context("completed GPU counters did not produce a valid timestamp span")
        }
        let tag = |generation| GpuFrameTag {
            downsize: 2.0 + generation as f32 * 0.125,
            viewport: [1920, 1080],
            generation,
        };
        for generation in 1..=SLOT_COUNT as u64 {
            submit(&mut timer, tag(generation))?;
        }
        ensure!(
            timer
                .begin(&handle.device, &handle.queue, tag(99))
                .is_none(),
            "a busy telemetry ring must skip instead of waiting"
        );
        assert_eq!(drain(&mut timer, handle)?.tag, tag(6));
        assert!(
            timer.poll(&handle.device).is_none(),
            "completed samples are delivered once"
        );

        // Rotate the allocation cursor once, then fill the ring. The newest
        // serial now occupies slot zero, not the last slot in iteration order.
        assert_eq!(submit(&mut timer, tag(7))?, 0);
        assert_eq!(drain(&mut timer, handle)?.tag, tag(7));
        let mut last_slot = usize::MAX;
        for generation in 8..=13 {
            last_slot = submit(&mut timer, tag(generation))?;
        }
        assert_eq!(last_slot, 0);
        assert_eq!(drain(&mut timer, handle)?.tag, tag(13));
        Ok(())
    }
}
