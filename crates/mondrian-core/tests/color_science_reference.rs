use mondrian_core::{delta_e_2000_d50, CieLabD50};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct ReferenceCorpus {
    schema_version: u32,
    source: String,
    citation: String,
    samples: Vec<ReferenceSample>,
}

#[derive(Debug, Deserialize)]
struct ReferenceSample {
    reference: [f64; 3],
    sample: [f64; 3],
    delta_e_2000: f64,
}

#[test]
fn ciede2000_matches_sharma_wu_dalal_supplemental_reference_data() {
    let corpus: ReferenceCorpus =
        serde_json::from_str(include_str!("reference/ciede2000_sharma_2005.json"))
            .expect("versioned CIEDE2000 reference corpus");

    assert_eq!(corpus.schema_version, 1);
    assert_eq!(corpus.samples.len(), 34);
    assert!(corpus.source.starts_with("https://hajim.rochester.edu/"));
    assert!(corpus.citation.contains("Sharma"));
    for (index, sample) in corpus.samples.iter().enumerate() {
        let reference = CieLabD50::new(
            sample.reference[0],
            sample.reference[1],
            sample.reference[2],
        )
        .expect("finite reference Lab");
        let observed = CieLabD50::new(sample.sample[0], sample.sample[1], sample.sample[2])
            .expect("finite sample Lab");
        let actual = delta_e_2000_d50(reference, observed).expect("finite CIEDE2000 result");

        assert!(
            (actual - sample.delta_e_2000).abs() <= 1.0e-4,
            "reference row {}: expected {}, observed {actual}",
            index + 1,
            sample.delta_e_2000
        );
    }
}
