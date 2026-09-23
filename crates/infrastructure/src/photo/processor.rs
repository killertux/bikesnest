//! Image processing (M4): decode → apply EXIF orientation → re-encode JPEG →
//! thumbnail. The raw upload is never stored; only these derivatives are.
//!
//! Decoding and the two JPEG encodes are pure CPU work on images of up to 20
//! megapixels, so they run on `tokio::task::spawn_blocking` — inline, a handful
//! of concurrent uploads would occupy every runtime worker and stall the whole
//! server, `/healthz` included. A [`Semaphore`] sized from explicit process
//! configuration bounds how many run at once, so an upload burst *queues* instead
//! of spawning an unbounded number of blocking tasks (each of which allocates
//! several times the decoded image).

use async_trait::async_trait;
use bikesnest_application::{PhotoError, ProcessedImage};
use bikesnest_domain::{PhotoDimensions, PhotoLimits};
use image::codecs::jpeg::JpegEncoder;
use image::imageops;
use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader};
use std::io::Cursor;
use std::sync::Arc;
use tokio::sync::Semaphore;

/// [`ImageProcessor`](bikesnest_application::ImageProcessor) via the `image`
/// crate. Deterministic, pure-Rust decode/re-encode for jpeg/png/webp inputs.
pub struct LocalImageProcessor {
    limits: PhotoLimits,
    /// Concurrency budget for the blocking decode/encode work.
    permits: Arc<Semaphore>,
    #[cfg(test)]
    blocking_hook: Option<Arc<dyn Fn() -> Box<dyn Send> + Send + Sync>>,
}

impl LocalImageProcessor {
    pub fn new(limits: PhotoLimits, concurrency: usize) -> Self {
        assert!(
            concurrency > 0,
            "image processing concurrency must be positive"
        );
        assert!(
            concurrency <= Semaphore::MAX_PERMITS,
            "image processing concurrency exceeds the semaphore implementation limit"
        );
        Self {
            limits,
            permits: Arc::new(Semaphore::new(concurrency)),
            #[cfg(test)]
            blocking_hook: None,
        }
    }

    #[cfg(test)]
    fn with_hook(
        limits: PhotoLimits,
        parallelism: usize,
        hook: Arc<dyn Fn() -> Box<dyn Send> + Send + Sync>,
    ) -> Self {
        Self {
            limits,
            permits: Arc::new(Semaphore::new(parallelism)),
            blocking_hook: Some(hook),
        }
    }

    fn is_allowed(format: ImageFormat) -> bool {
        matches!(
            format,
            ImageFormat::Jpeg | ImageFormat::Png | ImageFormat::WebP
        )
    }

    /// Map an EXIF orientation to the matching geometric transform.
    fn apply_orientation(rgb: image::RgbImage, orientation: Orientation) -> image::RgbImage {
        use Orientation::*;
        match orientation {
            NoTransforms => rgb,
            Rotate90 => imageops::rotate90(&rgb),
            Rotate180 => imageops::rotate180(&rgb),
            Rotate270 => imageops::rotate270(&rgb),
            FlipHorizontal => imageops::flip_horizontal(&rgb),
            FlipVertical => imageops::flip_vertical(&rgb),
            Rotate90FlipH => imageops::flip_horizontal(&imageops::rotate90(&rgb)),
            Rotate270FlipH => imageops::flip_horizontal(&imageops::rotate270(&rgb)),
        }
    }

    /// The whole pipeline, synchronously. Called only from a blocking task.
    fn process_blocking(limits: PhotoLimits, bytes: &[u8]) -> Result<ProcessedImage, PhotoError> {
        // Content sniff + cheap header read for the 20 MP cap (bomb defense).
        let reader = ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|_| PhotoError::Undecodable)?;

        if let Some(format) = reader.format()
            && !Self::is_allowed(format)
        {
            return Err(PhotoError::UnsupportedFormat);
        }

        let mut decoder = reader.into_decoder().map_err(|_| PhotoError::Undecodable)?;
        let (raw_w, raw_h) = decoder.dimensions();
        let dims = PhotoDimensions {
            width: raw_w,
            height: raw_h,
        };
        if !dims.within_limit(limits.max_megapixels) {
            return Err(PhotoError::TooManyPixels);
        }
        let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);

        // Decode, then flatten to RGB8 so every input color type encodes uniformly.
        let decoded = DynamicImage::from_decoder(decoder).map_err(|_| PhotoError::Undecodable)?;
        let rgb = decoded.to_rgb8();
        let oriented = Self::apply_orientation(rgb, orientation);
        let (width, height) = oriented.dimensions();

        // Full derivative: JPEG quality 85, no metadata (EXIF/ICC/XMP never
        // written). Constructing pixels from scratch guarantees the strip.
        let mut full = Vec::new();
        {
            let mut enc = JpegEncoder::new_with_quality(&mut full, limits.derivative_quality);
            enc.encode_image(&oriented)
                .map_err(|_| PhotoError::Undecodable)?;
        }

        // Thumbnail: longest side ≤ configured side, aspect preserved.
        let thumb_rgb = DynamicImage::ImageRgb8(oriented)
            .thumbnail(limits.thumb_max_side, limits.thumb_max_side)
            .to_rgb8();
        let mut thumb = Vec::new();
        {
            let mut enc = JpegEncoder::new_with_quality(&mut thumb, limits.derivative_quality);
            enc.encode_image(&thumb_rgb)
                .map_err(|_| PhotoError::Undecodable)?;
        }

        Ok(ProcessedImage {
            full,
            thumb,
            dimensions: PhotoDimensions { width, height },
            content_type: "image/jpeg",
        })
    }
}

#[async_trait]
impl bikesnest_application::ImageProcessor for LocalImageProcessor {
    async fn process(&self, bytes: &[u8]) -> Result<ProcessedImage, PhotoError> {
        // Wait for a slot *before* copying the upload onto a blocking thread,
        // so a burst queues here — holding nothing but the caller's own buffer
        // — instead of running N decodes at once. The semaphore is never
        // closed, so `acquire_owned` only fails if it were.
        let permit = self
            .permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| PhotoError::Undecodable)?;
        let limits = self.limits;
        let bytes = bytes.to_vec();
        #[cfg(test)]
        let hook = self.blocking_hook.clone();
        tokio::task::spawn_blocking(move || {
            // A started blocking task cannot be aborted. Keep its capacity
            // permit with the real decode/encode work, even if the caller is
            // cancelled while awaiting the JoinHandle.
            let _permit = permit;
            #[cfg(test)]
            let _work_guard = hook.map(|hook| hook());
            Self::process_blocking(limits, &bytes)
        })
        .await
        // A JoinError means the blocking pool panicked or is shutting down;
        // to the caller that is the same as an unusable upload.
        .map_err(|_| PhotoError::Undecodable)?
    }
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    use bikesnest_application::ImageProcessor;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Condvar, Mutex};

    #[test]
    fn constructor_rejects_out_of_range_concurrency() {
        assert!(
            std::panic::catch_unwind(|| LocalImageProcessor::new(PhotoLimits::default(), 0))
                .is_err()
        );
        assert!(
            std::panic::catch_unwind(|| {
                LocalImageProcessor::new(
                    PhotoLimits::default(),
                    Semaphore::MAX_PERMITS.saturating_add(1),
                )
            })
            .is_err()
        );
    }

    struct Gate {
        entered: AtomicUsize,
        active: AtomicUsize,
        peak: AtomicUsize,
        open: Mutex<bool>,
        wake: Condvar,
    }
    struct Active(Arc<Gate>);
    impl Drop for Active {
        fn drop(&mut self) {
            self.0.active.fetch_sub(1, Ordering::SeqCst);
        }
    }
    struct Release(Arc<Gate>);
    impl Drop for Release {
        fn drop(&mut self) {
            *self.0.open.lock().unwrap() = true;
            self.0.wake.notify_all();
        }
    }
    impl Gate {
        fn enter(self: &Arc<Self>) -> Box<dyn Send> {
            self.entered.fetch_add(1, Ordering::SeqCst);
            let now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(now, Ordering::SeqCst);
            let mut open = self.open.lock().unwrap();
            while !*open {
                open = self.wake.wait(open).unwrap();
            }
            drop(open);
            Box::new(Active(self.clone()))
        }
        fn release(&self) {
            *self.open.lock().unwrap() = true;
            self.wake.notify_all();
        }
    }

    #[tokio::test]
    async fn cancelled_image_caller_does_not_release_running_decode_capacity() {
        let gate = Arc::new(Gate {
            entered: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            open: Mutex::new(false),
            wake: Condvar::new(),
        });
        let _release = Release(gate.clone());
        let hook: Arc<dyn Fn() -> Box<dyn Send> + Send + Sync> = Arc::new({
            let gate = gate.clone();
            move || gate.enter()
        });
        let processor = Arc::new(LocalImageProcessor::with_hook(
            PhotoLimits::default(),
            1,
            hook,
        ));
        let first_processor = processor.clone();
        let first = tokio::spawn(async move { first_processor.process(b"bad image").await });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while gate.entered.load(Ordering::SeqCst) < 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        first.abort();
        let _ = first.await;
        let second_processor = processor.clone();
        let second = tokio::spawn(async move { second_processor.process(b"also bad").await });
        tokio::task::yield_now().await;
        assert_eq!(gate.entered.load(Ordering::SeqCst), 1);
        second.abort();
        let _ = second.await;
        let third_processor = processor.clone();
        let third = tokio::spawn(async move { third_processor.process(b"third bad").await });
        tokio::task::yield_now().await;
        assert_eq!(gate.entered.load(Ordering::SeqCst), 1);
        gate.release();
        third.await.unwrap().unwrap_err();
        assert_eq!(gate.peak.load(Ordering::SeqCst), 1);
        assert_eq!(gate.active.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn image_burst_counts_actual_blocking_closures_not_async_callers() {
        let gate = Arc::new(Gate {
            entered: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            open: Mutex::new(false),
            wake: Condvar::new(),
        });
        let _release = Release(gate.clone());
        let hook: Arc<dyn Fn() -> Box<dyn Send> + Send + Sync> = Arc::new({
            let gate = gate.clone();
            move || gate.enter()
        });
        let processor = Arc::new(LocalImageProcessor::with_hook(
            PhotoLimits::default(),
            2,
            hook,
        ));
        let mut encoded = std::io::Cursor::new(Vec::new());
        DynamicImage::new_rgb8(1024, 1024)
            .write_to(&mut encoded, ImageFormat::Png)
            .unwrap();
        let bytes = Arc::new(encoded.into_inner());
        let started = std::time::Instant::now();
        let tasks: Vec<_> = (0..6)
            .map(|_| {
                let processor = processor.clone();
                let bytes = bytes.clone();
                tokio::spawn(async move { processor.process(&bytes).await })
            })
            .collect();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while gate.entered.load(Ordering::SeqCst) < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(gate.peak.load(Ordering::SeqCst), 2);
        gate.release();
        for task in tasks {
            task.await.unwrap().unwrap();
        }
        assert_eq!(gate.peak.load(Ordering::SeqCst), 2);
        eprintln!(
            "image debug burst six elapsed_ms={}",
            started.elapsed().as_millis()
        );
    }
}
