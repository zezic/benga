#!/usr/bin/env python3
"""Evaluate production streaming detectors without copying music into the repo.

Requires ffmpeg and a release build of examples/detector_probe.rs. Output JSON
contains exact event times and descriptive proxies, never unlabeled accuracy.
"""
from __future__ import annotations

import argparse
import concurrent.futures
import hashlib
import json
from pathlib import Path
import random
import shutil
import statistics
import subprocess
import sys
import tempfile
import time


def select_tracks(library: Path, count: int, seed: int, matches: list[str]) -> list[Path]:
    tracks = sorted(p for p in library.rglob("*") if p.suffix.lower() in {".ogg", ".flac", ".mp3", ".wav", ".m4a"})
    if matches:
        return [p for p in tracks if any(m.casefold() in p.name.casefold() for m in matches)]
    # Uniform sampling from a sorted list is reproducible across filesystem order.
    return sorted(random.Random(seed).sample(tracks, min(count, len(tracks))))


def run_track(task: tuple, args: argparse.Namespace) -> dict:
    track, gain, sample_rate, fft_size, percussion_fft_size = task
    command = [str(args.probe.resolve()), "--sample-rate", str(sample_rate), "--gain-db", str(gain), "--label", track.name]
    if fft_size:
        command += ["--fft-size", str(fft_size)]
    if percussion_fft_size:
        command += ["--percussion-fft-size", str(percussion_fft_size)]
    if args.block_frames:
        command += ["--block-frames", str(args.block_frames)]
    trace_path = None
    if args.traces:
        digest = hashlib.sha256(f"{track.name}|{gain}|{sample_rate}|{fft_size}|{percussion_fft_size}".encode()).hexdigest()[:12]
        trace_path = args.traces / f"{digest}.jsonl"
        command += ["--trace", str(trace_path)]
    decoder = ["ffmpeg", "-nostdin", "-v", "error", "-i", str(track)]
    if args.seconds:
        decoder += ["-t", str(args.seconds)]
    decoder += ["-f", "f32le", "-ac", "2", "-ar", str(sample_rate), "pipe:1"]
    decoder_errors = tempfile.TemporaryFile()
    decode = subprocess.Popen(decoder, stdout=subprocess.PIPE, stderr=decoder_errors)
    try:
        analyzed = subprocess.run(command, stdin=decode.stdout, capture_output=True, text=True, timeout=args.timeout)
        decode.stdout.close()
        decode.wait(timeout=10)
        decoder_errors.seek(0)
        error = decoder_errors.read().decode(errors="replace")
        if analyzed.returncode or decode.returncode:
            raise RuntimeError(f"probe={analyzed.returncode}, decoder={decode.returncode}: {analyzed.stderr} {error}")
        report = json.loads(analyzed.stdout)
        if report["duration"] <= 0:
            raise RuntimeError("decoder produced no audio frames")
        report["fft_size"] = fft_size or "default"
        report["percussion_fft_size"] = percussion_fft_size or "default"
        if trace_path:
            report["trace"] = str(trace_path)
        # Distinct kick/snare pulse periodicity is only a diagnostic proxy. Music
        # may be syncopated, half-time, doubled, or have no regular backbeat.
        for name in ("kick", "snare", "hat"):
            times = report["detectors"][name]["times"]
            intervals = [b - a for a, b in zip(times, times[1:])]
            report["detectors"][name]["interval_cv"] = statistics.pstdev(intervals) / statistics.mean(intervals) if len(intervals) > 1 and statistics.mean(intervals) else None
        return report
    finally:
        if decode.poll() is None:
            decode.kill()
            decode.wait()
        decoder_errors.close()


def run_playlist(task: tuple, args: argparse.Namespace) -> dict:
    """Keep production detector history across decoded track boundaries."""
    tracks, gain, sample_rate, fft_size, percussion_fft_size = task
    command = [str(args.probe.resolve()), "--sample-rate", str(sample_rate),
               "--gain-db", str(gain), "--label", "continuous playlist"]
    if fft_size:
        command += ["--fft-size", str(fft_size)]
    if percussion_fft_size:
        command += ["--percussion-fft-size", str(percussion_fft_size)]
    if args.block_frames:
        command += ["--block-frames", str(args.block_frames)]
    if args.traces:
        digest = hashlib.sha256(f"playlist|{gain}|{sample_rate}|{fft_size}|{percussion_fft_size}".encode()).hexdigest()[:12]
        command += ["--trace", str(args.traces / f"{digest}.jsonl")]
    probe = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                             stderr=subprocess.PIPE)
    total_bytes, segments, failures = 0, [], []
    try:
        for track in tracks:
            decoder = ["ffmpeg", "-nostdin", "-v", "error", "-i", str(track)]
            if args.seconds:
                decoder += ["-t", str(args.seconds)]
            decoder += ["-f", "f32le", "-ac", "2", "-ar", str(sample_rate), "pipe:1"]
            start_bytes = total_bytes
            with tempfile.TemporaryFile() as decoder_errors:
                decode = subprocess.Popen(decoder, stdout=subprocess.PIPE, stderr=decoder_errors)
                try:
                    while chunk := decode.stdout.read(65536):
                        probe.stdin.write(chunk)
                        total_bytes += len(chunk)
                    decode.stdout.close()
                    code = decode.wait(timeout=args.timeout)
                    if code:
                        decoder_errors.seek(0)
                        failures.append({"track": track.name,
                                         "error": decoder_errors.read().decode(errors="replace")})
                finally:
                    if decode.poll() is None:
                        decode.kill()
                        decode.wait()
            if total_bytes > start_bytes:
                segments.append({"track": track.name,
                                 "start": start_bytes / 8 / sample_rate,
                                 "end": total_bytes / 8 / sample_rate,
                                 "decode_success": code == 0})
        probe.stdin.close()
        probe.stdin = None
        output, error = probe.communicate(timeout=args.timeout)
        if probe.returncode:
            raise RuntimeError(error.decode(errors="replace"))
        report = json.loads(output)
        report["fft_size"] = fft_size or "default"
        report["percussion_fft_size"] = percussion_fft_size or "default"
        for segment in segments:
            segment["counts"] = {
                name: sum(segment["start"] < at <= segment["end"]
                          for at in report["detectors"][name]["times"])
                for name in ("kick", "snare", "hat", "drop")
            }
        report["segments"] = segments
        report["decoder_failures"] = failures
        report["contains_partial_failed_decode"] = any(not segment["decode_success"] for segment in segments)
        return report
    finally:
        if probe.poll() is None:
            probe.kill()
            probe.wait()


def aggregate(reports: list[dict]) -> dict:
    duration = sum(r["duration"] for r in reports)
    out = {"runs": len(reports), "audio_seconds": duration}
    cpu_seconds = sum(r.get("cpu_seconds", 0) for r in reports)
    if any("cpu_seconds" in r for r in reports):
        out["cpu_seconds"] = cpu_seconds
        out["cpu_ms_per_audio_second"] = cpu_seconds * 1000 / duration if duration else None
    for name in ("kick", "snare", "hat", "drop"):
        rates = [r["detectors"][name]["per_second"] for r in reports]
        counts = [r["detectors"][name]["count"] for r in reports]
        out[name] = {"count": sum(counts), "median_rate": statistics.median(rates) if rates else None, "max_rate": max(rates, default=None), "zero_runs": sum(v == 0 for v in counts)}
        if any("candidates" in r["detectors"][name] for r in reports):
            out[name]["candidates"] = sum(r["detectors"][name].get("candidates", 0) for r in reports)
            out[name]["admitted"] = sum(r["detectors"][name].get("admitted", 0) for r in reports)
            out[name]["provisional_triggers"] = sum(r["detectors"][name].get("provisional_triggers", 0) for r in reports)
    return out


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--library", type=Path, default=Path.home() / "Music/Spotify Dump")
    parser.add_argument("--probe", type=Path, default=Path("target/release/examples/detector_probe"))
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--count", type=int, default=24)
    parser.add_argument("--seed", type=int, default=20261031)
    parser.add_argument("--match", action="append", default=[])
    parser.add_argument("--gains", type=float, nargs="+", default=[-6])
    parser.add_argument("--sample-rates", type=int, nargs="+", default=[48000])
    parser.add_argument("--fft-sizes", type=int, nargs="+", default=[0])
    parser.add_argument("--percussion-fft-sizes", type=int, nargs="+", default=[0], help="independent percussion FFT at 48 kHz: 256 or 512")
    parser.add_argument("--split", choices=["all", "calibration", "heldout"], default="all", help="fixed alternating partition of seeded sorted selection, formed before tuning")
    parser.add_argument("--seconds", type=float, help="analyze this much of each track; omitted means full track")
    parser.add_argument("--workers", type=int, default=4)
    parser.add_argument("--block-frames", type=int)
    parser.add_argument("--traces", type=Path)
    parser.add_argument("--timeout", type=float, default=240)
    parser.add_argument("--continuous", action="store_true",
                        help="retain detector history across the selected playlist")
    args = parser.parse_args()
    if not args.probe.exists():
        parser.error("probe missing; run cargo build --release --example detector_probe")
    tracks = select_tracks(args.library, args.count, args.seed, args.match)
    partition = {"calibration": [p.name for p in tracks[::2]], "heldout": [p.name for p in tracks[1::2]]}
    if args.split != "all":
        tracks = tracks[::2] if args.split == "calibration" else tracks[1::2]
    if not tracks:
        parser.error("no matching audio files")
    if args.traces:
        args.traces.mkdir(parents=True, exist_ok=True)
    sources = [tracks] if args.continuous else tracks
    tasks = [(p, g, r, f, pf) for p in sources for g in args.gains for r in args.sample_rates for f in args.fft_sizes for pf in args.percussion_fft_sizes]
    reports, errors = [], []
    started = time.monotonic()
    # Keep a single executable revision even if another terminal rebuilds Cargo
    # while the corpus is running. Its digest identifies the exact implementation.
    with tempfile.TemporaryDirectory(prefix="benga-detectors-") as probe_directory:
        copied_probe = Path(probe_directory) / "detector_probe"
        shutil.copy2(args.probe, copied_probe)
        probe_sha256 = hashlib.sha256(copied_probe.read_bytes()).hexdigest()
        args.probe = copied_probe
        with concurrent.futures.ThreadPoolExecutor(max_workers=args.workers) as pool:
            runner = run_playlist if args.continuous else run_track
            futures = {pool.submit(runner, task, args): task for task in tasks}
            for index, future in enumerate(concurrent.futures.as_completed(futures), 1):
                task = futures[future]
                label = "continuous playlist" if args.continuous else task[0].name
                try:
                    report = future.result()
                    reports.append(report)
                    errors.extend(report.get("decoder_failures", []))
                    counts = "/".join(str(report["detectors"][n]["count"]) for n in ("kick", "snare", "hat", "drop"))
                    print(f"[{index}/{len(tasks)}] {label} {task[1]:+g}dB {task[2]}Hz FFT{task[3] or 'default'}: K/S/H/D {counts}", file=sys.stderr)
                except Exception as error:
                    errors.append({"track": str(task[0]), "error": str(error)})
                    print(f"[{index}/{len(tasks)}] FAILED {label}: {error}", file=sys.stderr)
    reports.sort(key=lambda r: (r["label"], r["gain_db"], r["sample_rate"], str(r["fft_size"])))
    result = {
        "probe_sha256": probe_sha256,
        "seed": args.seed, "split": args.split, "partition": partition,
        "settings": {"gains_db": args.gains, "sample_rates": args.sample_rates,
                     "fft_sizes": args.fft_sizes, "percussion_fft_sizes": args.percussion_fft_sizes, "seconds": args.seconds,
                     "block_frames": args.block_frames, "continuous": args.continuous},
        "track_count": len(tracks), "tracks": [p.name for p in tracks],
        "wall_seconds": time.monotonic() - started,
        "aggregate": aggregate(reports), "errors": errors, "runs": reports,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps(result["aggregate"], indent=2))
    if errors:
        sys.exit(1)


if __name__ == "__main__":
    main()
