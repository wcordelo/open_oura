//! `oura-analysis` — the *interpretation (high level)* layer: turning decoded
//! samples into daily metrics, by porting Oura's on-phone `ecore` engine
//! (`libappecore.so`) algorithms. Each metric's provenance, formula and
//! validation status is documented under `docs/algorithms/`.
//!
//! Status: scaffolding + the first ported, validated algorithms (HRV RMSSD, SpO2
//! simple curve). Sleep summary, the three scores, baselines, temperature and
//! cycle are being ported from the decompiled ecore (see docs/algorithms/).
pub mod baseline;
pub mod hrv;
pub mod sleep;
pub mod sleep_debt;
pub mod spo2;
pub mod temperature;
