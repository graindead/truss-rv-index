//! truss-rv-index: scans RhythmVerse's Clone Hero charts into a compact index for Truss.
//! Used by the `truss-scan` command line tool and the `RV Cacher` window.

// Copied from Truss (see README for the commit); some of their helpers aren't used here.
#[allow(dead_code)]
pub mod chart;
#[allow(dead_code)]
pub mod chart_issues;
#[allow(dead_code)]
pub mod download;
#[allow(dead_code)]
pub mod fretwork;
#[allow(dead_code)]
pub mod rhythmverse;
#[allow(dead_code)]
pub mod sng;
#[allow(dead_code)]
pub mod trackhash;
#[allow(dead_code)]
pub mod zip_ranges;

pub mod api;
pub mod index;
pub mod record;
pub mod store;
pub mod zip_stream;

pub mod scan;
