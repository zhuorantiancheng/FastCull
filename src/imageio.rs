//! macOS ImageIO scaled JPEG decoding for reduced previews. Mirrors the
//! Windows WIC contract in native/jpeg_scaled.cpp: any unsupported or damaged
//! input stays a silent `None`, and the bounded Rust decoder remains the
//! authoritative fallback in `imaging`. ImageIO is thread-safe, so decode
//! workers may call this concurrently.

use crate::imaging::Pixels;
use std::ffi::c_void;

type CFIndex = isize;
type CFDataRef = *mut c_void;
type CFMutableDictionaryRef = *mut c_void;
type CFNumberRef = *mut c_void;
type CFStringRef = *const c_void;
type CGColorSpaceRef = *const c_void;
type CGContextRef = *mut c_void;
type CGImageRef = *mut c_void;
type CGImageSourceRef = *mut c_void;

const CF_NUMBER_CF_INDEX_TYPE: CFIndex = 14;
const CG_IMAGE_ALPHA_PREMULTIPLIED_LAST: u32 = 1;
const CG_IMAGE_BYTE_ORDER_32_BIG: u32 = 4 << 12;
const CG_IMAGE_STATUS_COMPLETE: i32 = 0;
const MAX_BUFFER_BYTES: u64 = 640_000_000;

#[repr(C)]
struct CGPoint {
    x: f64,
    y: f64,
}
#[repr(C)]
struct CGSize {
    width: f64,
    height: f64,
}
#[repr(C)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFBooleanTrue: *const c_void;
    static kCFBooleanFalse: *const c_void;
    fn CFDataCreate(allocator: *const c_void, bytes: *const u8, length: CFIndex) -> CFDataRef;
    fn CFDictionaryCreateMutable(
        allocator: *const c_void,
        capacity: CFIndex,
        keyCallbacks: *const c_void,
        valueCallbacks: *const c_void,
    ) -> CFMutableDictionaryRef;
    fn CFDictionarySetValue(dict: CFMutableDictionaryRef, key: *const c_void, value: *const c_void);
    fn CFNumberCreate(
        allocator: *const c_void,
        theType: CFIndex,
        valuePtr: *const c_void,
    ) -> CFNumberRef;
    fn CFRelease(cf: *const c_void);
}

#[link(name = "ImageIO", kind = "framework")]
unsafe extern "C" {
    static kCGImageSourceCreateThumbnailFromImageAlways: CFStringRef;
    static kCGImageSourceThumbnailMaxPixelSize: CFStringRef;
    static kCGImageSourceShouldCache: CFStringRef;
    static kCGImageSourceCreateThumbnailWithTransform: CFStringRef;
    fn CGImageSourceCreateWithData(data: CFDataRef, options: *const c_void) -> CGImageSourceRef;
    fn CGImageSourceGetStatus(source: CGImageSourceRef) -> i32;
    fn CGImageSourceCreateThumbnailAtIndex(
        source: CGImageSourceRef,
        index: usize,
        options: *const c_void,
    ) -> CGImageRef;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGBitmapContextCreate(
        data: *mut c_void,
        width: usize,
        height: usize,
        bitsPerComponent: usize,
        bytesPerRow: usize,
        space: CGColorSpaceRef,
        bitmapInfo: u32,
    ) -> CGContextRef;
    fn CGBitmapContextGetData(context: CGContextRef) -> *mut c_void;
    fn CGBitmapContextGetBytesPerRow(context: CGContextRef) -> usize;
    fn CGColorSpaceCreateDeviceRGB() -> CGColorSpaceRef;
    fn CGImageGetColorSpace(image: CGImageRef) -> CGColorSpaceRef;
    fn CGImageGetHeight(image: CGImageRef) -> usize;
    fn CGImageGetWidth(image: CGImageRef) -> usize;
    fn CGContextDrawImage(context: CGContextRef, rect: CGRect, image: CGImageRef);
}

/// Decode a JPEG into at least `width` × `height` RGBA8 pixels with ImageIO's
/// scaled decode, which skips inverse-DCT work like the Windows WIC path. The
/// returned dimensions can exceed the request by rounding; `imaging` refines
/// them with its own resize. Every failure returns `None`.
pub(crate) fn scaled_jpeg(bytes: &[u8], width: u32, height: u32) -> Option<Pixels> {
    if width == 0 || height == 0 || bytes.is_empty() {
        return None;
    }
    // SAFETY: every pointer passed to the C APIs belongs to an object created
    // inside this call; each create-rule reference is released on all exits,
    // the dictionary stores only immortal constants plus `number` (released
    // after the thumbnail request), and the pixel buffer is copied into an
    // owned `Vec` before `context` is released.
    unsafe {
        let data = CFDataCreate(std::ptr::null(), bytes.as_ptr(), bytes.len() as CFIndex);
        if data.is_null() {
            return None;
        }
        let source = CGImageSourceCreateWithData(data, std::ptr::null());
        CFRelease(data);
        if source.is_null() {
            return None;
        }
        // Truncated JPEGs can still yield a partial thumbnail; refuse them so
        // damaged input keeps going through the strict Rust decoder.
        if CGImageSourceGetStatus(source) != CG_IMAGE_STATUS_COMPLETE {
            CFRelease(source);
            return None;
        }
        let options =
            CFDictionaryCreateMutable(std::ptr::null(), 4, std::ptr::null(), std::ptr::null());
        if options.is_null() {
            CFRelease(source);
            return None;
        }
        let max_pixel_size: CFIndex = width.max(height) as CFIndex;
        let number = CFNumberCreate(
            std::ptr::null(),
            CF_NUMBER_CF_INDEX_TYPE,
            std::ptr::from_ref(&max_pixel_size).cast::<c_void>(),
        );
        if number.is_null() {
            CFRelease(options);
            CFRelease(source);
            return None;
        }
        // Decode from the full image: the EXIF thumbnail would under-resolve.
        CFDictionarySetValue(
            options,
            kCGImageSourceCreateThumbnailFromImageAlways,
            kCFBooleanTrue,
        );
        CFDictionarySetValue(options, kCGImageSourceThumbnailMaxPixelSize, number);
        CFDictionarySetValue(options, kCGImageSourceShouldCache, kCFBooleanFalse);
        // Orientation stays raw; the caller rotates after decode, like WIC.
        CFDictionarySetValue(
            options,
            kCGImageSourceCreateThumbnailWithTransform,
            kCFBooleanFalse,
        );
        let image = CGImageSourceCreateThumbnailAtIndex(source, 0, options);
        CFRelease(number);
        CFRelease(options);
        CFRelease(source);
        if image.is_null() {
            return None;
        }
        let (w, h) = (CGImageGetWidth(image), CGImageGetHeight(image));
        let Some(row) = w.checked_mul(4) else {
            CFRelease(image);
            return None;
        };
        let Some(expected) = row.checked_mul(h) else {
            CFRelease(image);
            return None;
        };
        if w == 0
            || h == 0
            || expected as u64 > MAX_BUFFER_BYTES
            || w < width as usize
            || h < height as usize
        {
            CFRelease(image);
            return None;
        }
        let space = CGImageGetColorSpace(image);
        let bitmap_info = CG_IMAGE_ALPHA_PREMULTIPLIED_LAST | CG_IMAGE_BYTE_ORDER_32_BIG;
        // The image's own color space keeps the decoded samples raw, matching
        // the unmanaged WIC and Rust paths: all preview levels display the
        // same encoded values. Grayscale or other non-RGB sources need an RGB
        // context; drawing maps gray levels to equal RGB channels, which is
        // what zune-jpeg does as well.
        let mut context =
            CGBitmapContextCreate(std::ptr::null_mut(), w, h, 8, row, space, bitmap_info);
        if context.is_null() {
            let rgb = CGColorSpaceCreateDeviceRGB();
            if rgb.is_null() {
                CFRelease(image);
                return None;
            }
            context = CGBitmapContextCreate(std::ptr::null_mut(), w, h, 8, row, rgb, bitmap_info);
            // The context retains the space; drop our create-rule reference.
            CFRelease(rgb);
            if context.is_null() {
                CFRelease(image);
                return None;
            }
        }
        // A bitmap context stores the first buffer row at the TOP of the
        // canvas, so a plain draw already lands top-down, matching CGImage
        // and JPEG row order. Verified with a native 2x2 red/blue probe:
        // flipping the CTM here mirrors the output vertically.
        CGContextDrawImage(
            context,
            CGRect {
                origin: CGPoint { x: 0.0, y: 0.0 },
                size: CGSize {
                    width: w as f64,
                    height: h as f64,
                },
            },
            image,
        );
        CFRelease(image);
        // Quartz may round up the allocated bytes-per-row when it owns the
        // buffer; copy row by row so the output stays tightly packed RGBA8.
        let stride = CGBitmapContextGetBytesPerRow(context);
        let base = CGBitmapContextGetData(context);
        let pixels = if base.is_null() || stride < row {
            None
        } else {
            let src = std::slice::from_raw_parts(base.cast::<u8>(), stride * h);
            let mut rgba = Vec::with_capacity(expected);
            for line in 0..h {
                let start = line * stride;
                rgba.extend_from_slice(&src[start..start + row]);
            }
            Some(rgba)
        };
        CFRelease(context);
        pixels.map(|rgba| Pixels {
            width: w,
            height: h,
            rgba,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sample the RGB triple at a relative position of the pixel buffer.
    fn rgb_at(p: &Pixels, fx: f64, fy: f64) -> [u8; 3] {
        let x = ((p.width as f64 - 1.0) * fx).round() as usize;
        let y = ((p.height as f64 - 1.0) * fy).round() as usize;
        let i = (y * p.width + x) * 4;
        [p.rgba[i], p.rgba[i + 1], p.rgba[i + 2]]
    }

    /// The asymmetric fixtures carry four quadrants (red/green top row,
    /// blue/yellow bottom row) plus distinct white top and dark bottom
    /// marker strips, so any vertical mirror or rotation shifts colors.
    fn assert_quadrants(p: &Pixels, context: &str) {
        let expected = [
            (0.3, 0.3, [255u8, 0, 0]),
            (0.7, 0.3, [0, 255, 0]),
            (0.3, 0.7, [0, 0, 255]),
            (0.7, 0.7, [255, 255, 0]),
        ];
        for (fx, fy, want) in expected {
            let got = rgb_at(p, fx, fy);
            assert!(
                got.iter().zip(want).all(|(a, b)| a.abs_diff(b) <= 45),
                "{context}: sample ({fx},{fy}) got {got:?}, want {want:?}"
            );
        }
    }

    #[test]
    fn imageio_scaled_jpeg_preserves_quadrant_layout_against_rust_reference() {
        for (name, bytes) in [
            (
                "asym-5120x2048.jpg",
                include_bytes!("../tests/fixtures/asym-5120x2048.jpg").as_slice(),
            ),
            (
                "asym-2048x5120.jpg",
                include_bytes!("../tests/fixtures/asym-2048x5120.jpg").as_slice(),
            ),
        ] {
            let reference = crate::imaging::decode_jpeg_bytes(bytes, crate::imaging::Level::Full)
                .expect("Rust reference decode");
            assert_quadrants(
                &Pixels {
                    width: reference.width,
                    height: reference.height,
                    rgba: reference.rgba.clone(),
                },
                &format!("{name} rust reference"),
            );
            // Proportional requests mirror what `imaging` derives from the
            // fixture edges: the long side targets 200 and 2560 exactly.
            let requests: [(u32, u32); 2] = if name == "asym-5120x2048.jpg" {
                [(200, 80), (2560, 1024)]
            } else {
                [(80, 200), (1024, 2560)]
            };
            for (width, height) in requests {
                let Some(p) = scaled_jpeg(bytes, width, height) else {
                    panic!("{name}: ImageIO scaled decode unavailable at {width}x{height}");
                };
                assert!(
                    p.width >= width as usize && p.height >= height as usize,
                    "{name}: {0}x{1} below request {width}x{height}",
                    p.width,
                    p.height
                );
                assert_quadrants(&p, &format!("{name} imageio {width}x{height}"));
            }
        }
    }

    #[test]
    fn imageio_scaled_jpeg_ignores_exif_orientation() {
        // Orientation 6 means "rotate 90 degrees on display"; with
        // WithTransform=false the stored row order must come back untouched,
        // so the red quadrant stays top-left and the reference agrees.
        let bytes = include_bytes!("../tests/fixtures/asym-exif6-5120x2048.jpg");
        let reference = crate::imaging::decode_jpeg_bytes(bytes, crate::imaging::Level::Full)
            .expect("Rust reference decode");
        assert_quadrants(
            &Pixels {
                width: reference.width,
                height: reference.height,
                rgba: reference.rgba.clone(),
            },
            "exif6 rust reference",
        );
        let Some(p) = scaled_jpeg(bytes, 200, 80) else {
            panic!("ImageIO scaled decode unavailable for the EXIF 6 fixture");
        };
        assert_quadrants(&p, "exif6 imageio");
    }

    #[test]
    fn production_scaled_jpeg_matches_rust_reference_across_levels() {
        // The production entry must resolve on macOS in tests; a None here
        // means the ImageIO path is missing and the fallback would hide it.
        let bytes = include_bytes!("../tests/fixtures/asym-5120x2048.jpg");
        let Some(p) = crate::native_raw::scaled_jpeg(bytes, 200, 80) else {
            panic!("production scaled_jpeg returned None; ImageIO path missing");
        };
        assert_quadrants(&p, "production entry 200px");
        let Some(p) = crate::native_raw::scaled_jpeg(bytes, 2560, 1024) else {
            panic!("production scaled_jpeg returned None at 2560px");
        };
        assert_quadrants(&p, "production entry 2560px");
        // Full stays a Rust decode; the levels must agree on the quadrant
        // layout (within JPEG decoder rounding) so switching resolutions
        // never mirrors the preview.
        let full = crate::imaging::decode_jpeg_bytes(bytes, crate::imaging::Level::Full)
            .expect("Rust full decode");
        let full = Pixels {
            width: full.width,
            height: full.height,
            rgba: full.rgba,
        };
        for (fx, fy) in [(0.3, 0.3), (0.7, 0.3), (0.3, 0.7), (0.7, 0.7)] {
            let scaled = rgb_at(&p, fx, fy);
            let reference = rgb_at(&full, fx, fy);
            assert!(
                scaled
                    .iter()
                    .zip(reference)
                    .all(|(a, b)| a.abs_diff(b) <= 45),
                "level layout differs at ({fx},{fy}): {scaled:?} vs {reference:?}"
            );
        }
    }

    #[test]
    fn imageio_scaled_jpeg_honors_dimension_contract() {
        let bytes = include_bytes!("../tests/fixtures/preview-3x2.jpg");
        for (width, height) in [(60, 40), (90, 60), (30, 20)] {
            let Some(p) = scaled_jpeg(bytes, width, height) else {
                panic!("ImageIO scaled decode failed at {width}x{height}");
            };
            assert!(
                p.width >= width as usize && p.height >= height as usize,
                "thumbnail {}x{} below request {width}x{height}",
                p.width,
                p.height
            );
            let ratio = p.width as f64 / p.height as f64;
            assert!((ratio - 1.5).abs() < 0.1, "aspect drifted: {ratio}");
            assert!(p.rgba.chunks_exact(4).all(|px| px[3] == 255));
        }
    }

    #[test]
    fn imageio_scaled_jpeg_rejects_malformed_truncated_and_empty_input() {
        assert!(scaled_jpeg(b"broken jpeg", 8, 6).is_none());
        assert!(scaled_jpeg(b"", 8, 6).is_none());
        let bytes = include_bytes!("../tests/fixtures/tiny.jpg");
        assert!(scaled_jpeg(&bytes[..bytes.len() / 2], 8, 6).is_none());
        assert!(scaled_jpeg(bytes, 0, 6).is_none());
    }

    #[test]
    fn imageio_scaled_jpeg_is_repeatable_across_calls() {
        let bytes = include_bytes!("../tests/fixtures/tiny.jpg");
        let first = scaled_jpeg(bytes, 8, 6).expect("ImageIO scaled decode");
        for _ in 0..8 {
            let again = scaled_jpeg(bytes, 8, 6).expect("repeated decode");
            assert_eq!((first.width, first.height), (again.width, again.height));
            assert_eq!(first.rgba, again.rgba);
        }
    }
}
