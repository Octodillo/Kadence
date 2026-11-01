use crate::WorkerManager;
use lv2_raw::LV2Feature;
use lv2_sys::{LV2_BUF_SIZE__boundedBlockLength, LV2_STATE__loadDefaultState};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;
use std::{collections::HashSet, ffi::CStr};

pub mod options;
pub mod urid_map;
pub mod worker;

/// A builder for `Features` objects.
#[derive(Clone, Debug)]
pub struct FeaturesBuilder {
    /// The minimum block size. If plugins try to process less samples than this
    /// on a single `run` call, an error will be returned.
    pub min_block_length: usize,
    /// The maximum block size. If plugins try to process more samples than this
    /// on a single `run` call, an error will be returned.
    pub max_block_length: usize,
}

impl Default for FeaturesBuilder {
    fn default() -> FeaturesBuilder {
        FeaturesBuilder {
            min_block_length: 1,
            max_block_length: 4096,
        }
    }
}

impl FeaturesBuilder {
    /// Build features, panicking if block bounds are invalid.
    ///
    /// Use `try_build` to receive a configuration error instead.
    #[deprecated(since = "0.9.0", note = "use try_build to handle invalid block bounds")]
    pub fn build(self, world: &crate::World) -> Arc<Features> {
        self.try_build(world).expect("invalid feature block bounds")
    }

    /// Build features, returning an error before creating features or starting
    /// a worker thread if either bound is unrepresentable or out of order.
    pub fn try_build(
        self,
        _world: &crate::World,
    ) -> Result<Arc<Features>, crate::error::FeatureError> {
        use crate::error::FeatureError;
        if self.min_block_length > self.max_block_length {
            return Err(FeatureError::ReversedBounds {
                min: self.min_block_length,
                max: self.max_block_length,
            });
        }
        let min = i32::try_from(self.min_block_length).map_err(|_| {
            FeatureError::BlockLengthOutOfRange {
                value: self.min_block_length,
                min: 0,
                max: i32::MAX,
            }
        })?;
        let max = i32::try_from(self.max_block_length).map_err(|_| {
            FeatureError::BlockLengthOutOfRange {
                value: self.max_block_length,
                min: 0,
                max: i32::MAX,
            }
        })?;
        let worker_manager = Arc::new(WorkerManager::default());
        let worker_thread = WorkerThread::spawn(worker_manager.clone());
        let mut features = Features {
            urid_map: urid_map::UridMap::new(),
            options: options::Options::new(),
            min_block_length: self.min_block_length,
            max_block_length: self.max_block_length,
            bounded_block_length: LV2Feature {
                uri: LV2_BUF_SIZE__boundedBlockLength.as_ptr().cast(),
                data: std::ptr::null_mut(),
            },
            load_default_state: LV2Feature {
                uri: LV2_STATE__loadDefaultState.as_ptr().cast(),
                data: std::ptr::null_mut(),
            },
            worker_manager,
            _worker_thread: worker_thread,
        };
        features.options.set_int_option(
            &features.urid_map,
            features
                .urid_map
                .map(c"http://lv2plug.in/ns/ext/buf-size#minBlockLength"),
            min,
        );
        features.options.set_int_option(
            &features.urid_map,
            features
                .urid_map
                .map(c"http://lv2plug.in/ns/ext/buf-size#maxBlockLength"),
            max,
        );
        Ok(Arc::new(features))
    }
}

/// `Features` are used to provide functionality to plugins.
pub struct Features {
    pub(crate) urid_map: Pin<Box<urid_map::UridMap>>,
    options: options::Options,
    bounded_block_length: LV2Feature,
    load_default_state: LV2Feature,
    min_block_length: usize,
    max_block_length: usize,
    worker_manager: Arc<WorkerManager>,
    _worker_thread: WorkerThread,
}

unsafe impl Send for Features {}
unsafe impl Sync for Features {}

impl Features {
    /// Get the URIs for all supported features.
    pub fn supported_features() -> HashSet<&'static str> {
        HashSet::from([
            "http://lv2plug.in/ns/ext/urid#map",
            "http://lv2plug.in/ns/ext/urid#unmap",
            "http://lv2plug.in/ns/ext/options#options",
            "http://lv2plug.in/ns/ext/buf-size#boundedBlockLength",
            "http://lv2plug.in/ns/ext/worker#schedule",
            "http://lv2plug.in/ns/ext/state#loadDefaultState",
        ])
    }

    /// Iterate over all the LV2 features.
    pub fn iter_features<'a>(
        &'a self,
        worker_feature: &'a LV2Feature,
    ) -> impl Iterator<Item = &'a LV2Feature> {
        std::iter::once(self.urid_map.as_urid_map_feature())
            .chain(std::iter::once(self.urid_map.as_urid_unmap_feature()))
            .chain(std::iter::once(self.options.as_feature()))
            .chain(std::iter::once(&self.bounded_block_length))
            .chain(std::iter::once(&self.load_default_state))
            .chain(std::iter::once(worker_feature))
    }

    /// The minimum allowed block length.
    pub fn min_block_length(&self) -> usize {
        self.min_block_length
    }

    /// The maximum allowed block length.
    pub fn max_block_length(&self) -> usize {
        self.max_block_length
    }

    /// The urid for the given uri.
    pub fn urid(&self, uri: &CStr) -> u32 {
        self.urid_map.map(uri)
    }

    /// The urid for midi.
    pub fn midi_urid(&self) -> lv2_raw::LV2Urid {
        self.urid(c"http://lv2plug.in/ns/ext/midi#MidiEvent")
    }

    /// The uri for the given urid.
    pub fn uri(&self, urid: lv2_raw::LV2Urid) -> Option<&str> {
        self.urid_map.unmap(urid)
    }

    /// The worker manager. This is automatically run periodically to perform
    /// any asynchronous work that plugins have scheduled.
    pub fn worker_manager(&self) -> &Arc<WorkerManager> {
        &self.worker_manager
    }
}

impl std::fmt::Debug for Features {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Features")
            .field("urid_map", &self.urid_map)
            .field("options", &self.options)
            .field("bounded_block_length", &"__uri__")
            .field("load_default_state", &"__uri__")
            .field("min_block_length", &self.min_block_length)
            .field("max_block_length", &self.max_block_length)
            .field("worker_manager", &self.worker_manager)
            .field("_worker_thread", &self._worker_thread)
            .finish()
    }
}

/// How long the worker thread waits between runs of the plugins' workers.
const WORKER_PERIOD: Duration = Duration::from_millis(100);

/// The thread that runs plugin workers for a `Features`. Dropping it stops the
/// thread and joins it.
///
/// It is joined, never detached: glibc's `pthread_detach` (2.39, as on Ubuntu
/// 24.04) reads the thread's descriptor after marking it detached, and a thread
/// that exits at that moment frees the descriptor with its stack, so the read
/// can fault. A thread that is stopped and joined has finished exiting first.
#[derive(Debug)]
struct WorkerThread {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl WorkerThread {
    fn spawn(workers: Arc<WorkerManager>) -> WorkerThread {
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let handle = std::thread::Builder::new()
            .name("livi-worker".into())
            .spawn(move || {
                while !stopped.load(Ordering::Acquire) {
                    workers.run_workers();
                    // Woken early by `drop`.
                    std::thread::park_timeout(WORKER_PERIOD);
                }
            })
            // PANIC OK: as `std::thread::spawn` does.
            .expect("failed to spawn the LV2 worker thread");
        WorkerThread {
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for WorkerThread {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            handle.thread().unpark();
            // A panic on the worker thread was already reported by the panic hook.
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_feature_bounds_return_errors() {
        let world = crate::World::with_plugin_predicate(|_| false);
        assert!(
            FeaturesBuilder {
                min_block_length: 256,
                max_block_length: 1,
            }
            .try_build(&world)
            .is_err(),
            "accepted bounds 256..=1"
        );
        assert!(
            FeaturesBuilder {
                min_block_length: 1,
                max_block_length: i32::MAX as usize + 1,
            }
            .try_build(&world)
            .is_err(),
            "accepted bounds 1..={}",
            i32::MAX as usize + 1
        );
        assert!(
            FeaturesBuilder {
                min_block_length: i32::MAX as usize + 1,
                max_block_length: i32::MAX as usize + 1,
            }
            .try_build(&world)
            .is_err(),
            "accepted bounds {0}..={0}",
            i32::MAX as usize + 1
        );
    }

    // Kadence issue #152: dropping the features stops their worker thread and
    // joins it, rather than detaching a thread that may be exiting.
    #[test]
    fn dropping_features_joins_the_worker_thread() {
        let world = crate::World::with_plugin_predicate(|_| false);
        for _ in 0..100 {
            let features = FeaturesBuilder::default().try_build(&world).unwrap();
            let workers = Arc::downgrade(features.worker_manager());
            drop(features);
            // The thread held the other reference; it has ended.
            assert!(
                workers.upgrade().is_none(),
                "the worker thread outlived its features"
            );
        }
    }

    #[test]
    fn representable_ordered_feature_bounds_are_accepted() {
        let world = crate::World::with_plugin_predicate(|_| false);
        for (min, max) in [(0, 0), (1, 1), (1, i32::MAX as usize)] {
            let features = FeaturesBuilder {
                min_block_length: min,
                max_block_length: max,
            }
            .try_build(&world)
            .unwrap();
            assert_eq!(features.min_block_length(), min);
            assert_eq!(features.max_block_length(), max);
        }
    }
}
