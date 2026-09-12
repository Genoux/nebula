//! Dynamic time warping: how well a recorded phrase appears somewhere inside a
//! window of live audio, allowing it to be spoken faster or slower.
//!
//! Subsequence DTW rather than the textbook global form. The phrase takes up
//! perhaps half of the window being searched, and global DTW insists on
//! aligning the template against the silence around it too - measured, that
//! doubled the distance of a genuine match. Here the template must be matched
//! end to end, but it may start and finish anywhere in the window.

/// Mean per-template-frame cost of the best placement of `template` inside
/// `window`, or `None` if either is empty.
///
/// Normalised by template length so one threshold works for "nebula" and for
/// "hey nebula" alike.
pub fn best_match(template: &[Vec<f32>], window: &[Vec<f32>]) -> Option<f32> {
    if template.is_empty() || window.is_empty() {
        return None;
    }
    let m = window.len();

    // Row 0 is all zeros: that is what lets the match begin at any window
    // frame. Two rows, because the recurrence only looks one row back and this
    // runs ten times a second.
    let mut previous = vec![0.0f32; m + 1];
    let mut current = vec![f32::INFINITY; m + 1];

    for frame in template {
        current[0] = f32::INFINITY;
        for j in 1..=m {
            let cost = euclidean(frame, &window[j - 1]);
            current[j] = cost + previous[j - 1].min(previous[j]).min(current[j - 1]);
        }
        std::mem::swap(&mut previous, &mut current);
    }

    // The minimum over the last row is what lets the match end anywhere.
    let best = previous[1..].iter().copied().fold(f32::INFINITY, f32::min);
    best.is_finite().then(|| best / template.len() as f32)
}

fn euclidean(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| (x - y) * (x - y))
        .sum::<f32>()
        .sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seq(values: &[f32]) -> Vec<Vec<f32>> {
        values.iter().map(|&v| vec![v, v]).collect()
    }

    #[test]
    fn a_sequence_matches_itself_exactly() {
        let s = seq(&[1.0, 2.0, 3.0, 4.0]);
        assert_eq!(best_match(&s, &s), Some(0.0));
    }

    #[test]
    fn empty_input_has_no_match() {
        assert_eq!(best_match(&[], &seq(&[1.0])), None);
        assert_eq!(best_match(&seq(&[1.0]), &[]), None);
    }

    #[test]
    fn the_phrase_is_found_inside_surrounding_silence() {
        // The case global DTW got wrong: the phrase is exactly present, just
        // not alone.
        let phrase = seq(&[5.0, 7.0, 9.0]);
        let window = seq(&[0.0, 0.0, 0.0, 5.0, 7.0, 9.0, 0.0, 0.0]);
        assert_eq!(best_match(&phrase, &window), Some(0.0));
    }

    #[test]
    fn a_stretched_phrase_still_matches_closely() {
        let fast = seq(&[1.0, 4.0, 8.0]);
        let slow_in_window = seq(&[0.0, 1.0, 1.0, 4.0, 4.0, 8.0, 8.0, 0.0]);
        let stretched = best_match(&fast, &slow_in_window).unwrap();
        let absent = best_match(&fast, &seq(&[0.0, 0.0, 0.0, 0.0])).unwrap();
        assert!(stretched < absent, "{stretched} should beat {absent}");
        assert!(stretched < 0.1, "time-stretching alone cost {stretched}");
    }

    #[test]
    fn a_different_phrase_scores_worse_than_a_similar_one() {
        let template = seq(&[1.0, 2.0, 3.0]);
        let close = best_match(&template, &seq(&[0.0, 1.1, 2.1, 3.1, 0.0])).unwrap();
        let far = best_match(&template, &seq(&[8.0, 1.0, 9.0, 8.0, 9.0])).unwrap();
        assert!(close < far, "{close} vs {far}");
    }

    #[test]
    fn collapsing_the_template_onto_one_frame_is_not_free() {
        // Every template frame must be paid for, so a single loud window frame
        // cannot absorb a whole phrase.
        let template = seq(&[1.0, 5.0, 9.0]);
        let d = best_match(&template, &seq(&[5.0])).unwrap();
        assert!(d > 2.0, "three distinct frames matched one frame for {d}");
    }
}
