//! Plain-language hover help for the live detector controls.
use super::{ArrangementSetting, Parameter, PercussionSetting};

impl PercussionSetting {
    pub fn hint(self) -> &'static str {
        match self {
            Self::LowHz => "Lowest frequency this instrument listens to. Raising it ignores more low-frequency sound.",
            Self::HighHz => "Upper edge of the fully weighted frequency band. Sound above it fades to zero at the fade-end frequency.",
            Self::TaperHz => "Frequency where the selected band's weight reaches zero. Set it equal to the upper frequency to skip the fade.",
            Self::FeatureMs => "How long each attack is measured before its decision. Longer windows include more of the attack and delay the result.",
            Self::ConcentrationMs => "Short slice used to score how tightly the combined upward movement rises. Larger slices count broader rises as concentrated and usually raise the score.",
            Self::EnvelopeMs => "Smooths left and right levels separately before their upward movement is combined. Higher values reduce small fluctuations and soften sharp rises.",
            Self::BackgroundMs => "How quickly each side’s background estimate follows changing sound. Larger values retain the earlier background longer.",
            Self::MergeMs => "Separation used when splitting a new rise from a nearby attack. Larger values combine more nearby peaks.",
            Self::RearmMs => "How long the rise must settle before an ordinary fresh attack can begin. Higher values make the detector wait longer between candidates.",
            Self::AdmissionEnergy => "Minimum new energy needed before an attack can teach the history. Higher values keep weaker new attacks out; changes affect future learning while existing history ages out normally.",
            Self::AdmissionGrowth => "Minimum combined upward movement from left and right needed before learning. Higher values reject smaller new rises; changes affect future learning while existing history ages out normally.",
            Self::AdmissionProminence => "Minimum new rise compared with the attack's peak before it can be learned. Higher values require clearer new attacks; changes affect future learning while existing history ages out normally.",
            Self::MinSharpness => "Fixed lower limit on how concentrated the left/right upward movement must be to fire. Higher values require sharper attacks, even when recent history is mostly smooth.",
            Self::MinOutputProminence => "How clearly a new attack must rise above the existing sound to fire. It can still be learned when this trigger check fails.",
            Self::MinAttackDb => "Minimum strength of the new attack required to fire. A less negative dBFS value requires a stronger attack.",
            Self::OutputRmsDb => "Minimum overall music level required to fire. A less negative value requires louder input; this check does not block learning.",
            Self::SharpnessPercentile => "Sets the sharpness cut among recent learned attacks. Higher values are stricter; the incoming attack is checked before it joins that history. Zero skips this comparison.",
            Self::EnergyPercentile => "Sets how strong an attack must be compared with recent learned attacks. Higher values are stricter; this does not decide which attacks are learned. Zero skips this comparison.",
            Self::MemorySeconds => "How long learned attacks remain in comparison history. Larger values keep older examples and adapt more slowly.",
            Self::MinReference => "How many learned attacks are needed before the percentage comparisons start. Earlier attacks can still fire when the other checks pass.",
            Self::OutputIntervalMs => "Minimum time between this instrument's triggers. Attacks inside this delay can still teach the history.",
            Self::FftSize => "Size of the frequency-measuring window for snare and hat. Larger values separate frequencies better but blur timing; kick uses its own window.",
            Self::HopMs => "Time between measurements, shared by all percussion detectors. Smaller values give finer timing and use more CPU.",
            Self::RecentMarkers => "How many recent learned additions are marked on the heatmap. This changes only the display; expired reference points can remain visible as history.",
            Self::InputEnergyEnabled => "Turn the minimum-energy check for learning on or off. The saved floor value and all other checks stay unchanged.",
            Self::InputGrowthEnabled => "Turn the minimum-rise check for learning on or off. The saved floor value and all other checks stay unchanged.",
            Self::InputProminenceEnabled => "Turn the minimum-contrast check for learning on or off. The saved floor value and all other checks stay unchanged.",
            Self::MinSharpnessEnabled => "Turn the fixed sharpness floor for triggers on or off. The recent sharpness comparison still applies when its switch is on.",
            Self::OutputProminenceEnabled => "Turn the trigger's contrast check on or off. Input filtering and the other trigger checks stay unchanged.",
            Self::AttackLevelEnabled => "Turn the fixed attack-strength floor for triggers on or off. The recent energy comparison can still reject weaker attacks.",
            Self::MixRmsEnabled => "Turn the overall music-level check for triggers on or off. The other input and trigger checks still apply.",
            Self::SharpnessPercentileEnabled => "Turn only the recent sharpness comparison for triggers on or off. Learning continues and the saved percentage stays unchanged.",
            Self::EnergyPercentileEnabled => "Turn only the recent energy comparison for triggers on or off. Learning continues and the saved percentage stays unchanged.",
            Self::OutputIntervalEnabled => "Turn the minimum delay between triggers on or off. The saved delay is kept for when this check is switched on again.",
            Self::RejectOverlap => "When on, attacks with overlapping measurement windows are rejected before learning. When off, both may be learned even though their measurements can include both sounds.",
        }
    }
}

impl ArrangementSetting {
    pub fn hint(self) -> &'static str {
        match self {
            Self::MixLossDb => "Required drop of the smoothed mix below its learned reference. Both the shape-based path and the independent mix-loss path use this level. On the energy-loss graph, positive dB means quieter and the solid line must rise above the dashed requirement.",
            Self::MixLossHold => "How long the measured mix loss must continuously exceed Mix loss to confirm a gap on its own. Shape changes and the RMS gap condition are not required. Reaching or falling below the required loss resets this timer; it is independent of Gap hold and RMS gap hold. A confirmed gap stays armed until a valid drop consumes it.",
            Self::ShapeDistance => "How different the frequency balance must become before a gap can start. Higher values require a bigger change.",
            Self::TrendChange => "How much the pattern across neighboring frequency areas must change to support a gap. Higher values require a stronger change.",
            Self::ChangedFraction => "How many frequency areas must change together to support a gap. Higher values require a wider change.",
            Self::GapHold => "How long the shape-based gap conditions must remain present together before that path confirms a gap. Higher values ignore shorter musical pauses. Mix loss hold and RMS gap hold use their own independent timers. Once confirmed, the gap stays armed until a drop consumes it.",
            Self::GapRmsDb => "Independent way to confirm a gap: the overall analysis-window RMS after Boost must stay strictly below this dBFS level for RMS gap hold. Shape changes, mix loss and reference warmup are not required. A confirmed gap stays armed until a valid drop consumes it.",
            Self::GapRmsHold => "How long overall RMS must stay continuously below RMS gap below to confirm a gap on its own. Reaching or exceeding that level resets this timer. This hold is independent of the shape-based Gap hold and the independent Mix loss hold.",
            Self::ReferenceTime => "How quickly the arrangement reference follows the current section. Higher values remember the earlier section for longer.",
            Self::BassFastTime => "How quickly the fast low-frequency envelope follows the bass level. Larger times smooth short hits and respond more slowly. It must stay at least 5 ms faster than the slow envelope; dragging this control keeps the slow setting unchanged.",
            Self::BassSlowTime => "How quickly the slow low-frequency envelope follows the bass background. Larger times remember earlier bass levels for longer and keep new rises visible longer. It must stay at least 5 ms slower than the fast envelope; dragging this control keeps the fast setting unchanged.",
            Self::BassHitThreshold => "Required fraction of the fast low-frequency envelope above the slow envelope. Higher values require a larger rise; the solid fast line must reach the dashed requirement. Fast and slow envelope controls set the response times. A fresh LF rise must begin after the gap is confirmed.",
            Self::DropMinRmsDb => "Minimum overall music level needed to confirm a drop after a gap and a low-frequency envelope rise. Less negative values require a louder return. The RMS readout shows whether this gate passes.",
            Self::DropRefractory => "Minimum time between drop triggers. Higher values prevent rapid repeated scene changes.",
        }
    }
}

impl Parameter {
    pub fn analysis_hint(self) -> &'static str {
        match self {
            Self::Percussion(_, setting) => setting.hint(),
            Self::Arrangement(setting) => setting.hint(),
            Self::OnsetBinGain(_, _) => "Reduce this frequency's contribution to the selected instrument. At 100% it is unchanged; at 0% it is ignored. The playing music is unaffected.",
            _ => "Adjust this setting by dragging its slider. Its value is saved automatically.",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::visualizer::Detector;

    #[test]
    fn every_detector_control_has_specific_plain_hover_help() {
        for setting in PercussionSetting::CHANNEL.into_iter().chain([
            PercussionSetting::FftSize,
            PercussionSetting::HopMs,
            PercussionSetting::RecentMarkers,
        ]) {
            let hint = setting.hint();
            assert!(!hint.is_empty(), "{setting:?}");
            assert!(
                !hint.to_lowercase().contains("admission"),
                "{setting:?}: {hint}"
            );
            assert_eq!(
                Parameter::Percussion(Detector::Kick, setting).analysis_hint(),
                hint
            );
        }
        for setting in ArrangementSetting::ALL {
            let hint = setting.hint();
            assert!(!hint.is_empty(), "{setting:?}");
            assert_eq!(Parameter::Arrangement(setting).analysis_hint(), hint);
        }
    }

    #[test]
    fn independent_filter_hints_explain_their_switches_and_learning_scope() {
        let input_checks = [
            PercussionSetting::InputEnergyEnabled,
            PercussionSetting::InputGrowthEnabled,
            PercussionSetting::InputProminenceEnabled,
        ];
        for setting in input_checks {
            assert!(setting.hint().contains("for learning on or off"));
            assert!(setting.hint().contains("saved floor value"));
        }
        let trigger_checks = [
            PercussionSetting::MinSharpnessEnabled,
            PercussionSetting::OutputProminenceEnabled,
            PercussionSetting::AttackLevelEnabled,
            PercussionSetting::MixRmsEnabled,
            PercussionSetting::SharpnessPercentileEnabled,
            PercussionSetting::EnergyPercentileEnabled,
            PercussionSetting::OutputIntervalEnabled,
        ];
        for setting in trigger_checks {
            assert!(setting.hint().contains("on or off"), "{setting:?}");
            assert!(setting.hint().contains("trigger"), "{setting:?}");
        }
        assert!(PercussionSetting::RejectOverlap
            .hint()
            .contains("overlapping"));
        assert!(PercussionSetting::EnergyPercentile
            .hint()
            .contains("does not decide which attacks are learned"));
        assert!(PercussionSetting::MinReference
            .hint()
            .contains("Earlier attacks can still fire"));
    }
}
