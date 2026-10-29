//! Row-band parallelism for the spatial kernels.
//!
//! Every spatial kernel in this crate is row independent once its tap grid is
//! hoisted: a row's output depends only on the source frame, never on other
//! output rows. Splitting the destination into row bands therefore preserves
//! the per-pixel arithmetic (and the result bit for bit) while using the
//! machine's cores, which is the cheapest real speedup available on the CPU
//! path.
//!
//! Point kernels deliberately stay serial: measured at 300-890 MP/s they are
//! already at the single-thread DRAM stream rate, so a split only adds thread
//! spawn cost (channel mixer regressed from 1.4 ms to 3.1 ms when split).

use std::ops::Range;

/// Threads used for one kernel call: enough to fill the machine, capped so a
/// small frame still amortizes the spawn cost.
pub(crate) fn worker_count(rows: usize) -> usize {
    let available = std::thread::available_parallelism().map(|count| count.get()).unwrap_or(1);
    // Below this many rows the split costs more than it saves.
    const MIN_ROWS_PER_WORKER: usize = 8;
    available.min(rows / MIN_ROWS_PER_WORKER).max(1)
}

/// Runs `apply` over disjoint row bands of `frame` in parallel.
///
/// `apply` receives the first row of its band and that band's mutable rows.
/// A panic in a worker propagates because the scope re-panics.
pub(crate) fn for_each_row_band<F>(
    frame: &mut [[f32; 4]],
    width: usize,
    rows: Range<usize>,
    apply: F,
) where
    F: Fn(usize, &mut [[f32; 4]]) + Sync,
{
    let workers = worker_count(rows.len());
    if workers <= 1 || width == 0 {
        apply(rows.start, &mut frame[rows.start * width..rows.end * width]);
        return;
    }
    let rows_per_band = rows.len().div_ceil(workers);
    let band_end = rows.end;
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        let mut band_start = rows.start;
        let offset = rows.start * width;
        let mut rest = &mut frame[offset..rows.end * width];
        while band_start < band_end {
            let band_rows = rows_per_band.min(band_end - band_start);
            let (band, tail) = rest.split_at_mut(band_rows * width);
            rest = tail;
            let apply = &apply;
            handles.push(scope.spawn(move || apply(band_start, band)));
            band_start += band_rows;
        }
        for handle in handles {
            if let Err(panic) = handle.join() {
                std::panic::resume_unwind(panic);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_band_parallelism_covers_every_row_exactly_once() {
        // 256 rows with the default 8-row floor puts every core to work, so
        // this exercises the split path rather than the serial fallback.
        const WIDTH: usize = 4;
        const HEIGHT: usize = 256;
        let mut frame = vec![[0.0_f32; 4]; WIDTH * HEIGHT];
        for_each_row_band(&mut frame, WIDTH, 0..HEIGHT, |row_start, band| {
            for (offset, chunk) in band.chunks_mut(WIDTH).enumerate() {
                let row = (row_start + offset) as f32;
                for pixel in chunk {
                    *pixel = [row, row + 1.0, row + 2.0, 1.0];
                }
            }
        });
        for row in 0..HEIGHT {
            for column in 0..WIDTH {
                let pixel = frame[row * WIDTH + column];
                assert_eq!(pixel[0], row as f32, "row {row} column {column}");
                assert_eq!(pixel[1], row as f32 + 1.0);
                assert_eq!(pixel[2], row as f32 + 2.0);
                assert_eq!(pixel[3], 1.0);
            }
        }
    }

    #[test]
    fn worker_count_stays_within_the_machine_and_the_floor() {
        assert_eq!(worker_count(0), 1);
        assert_eq!(worker_count(7), 1);
        let available = std::thread::available_parallelism().map(|count| count.get()).unwrap_or(1);
        // Eight rows is exactly the per-worker floor, so one worker is used.
        assert_eq!(worker_count(8), 1);
        assert!(worker_count(100_000) <= available);
    }
}
