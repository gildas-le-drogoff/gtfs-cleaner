use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufReader, Cursor, Read, Write};

use gzp::ZWriter;
use gzp::deflate::Gzip;
use gzp::par::compress::{ParCompress, ParCompressBuilder};
use rayon::prelude::*;
use zip::{ZipArchive, ZipWriter};

const TARGET_FILES: [&str; 3] = ["stop_times.txt", "trips.txt", "frequencies.txt"];

fn base_name(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

fn nth_field(line: &str, idx: usize) -> Option<&str> {
    let mut start = 0;
    let mut count = 0;
    for (i, b) in line.bytes().enumerate() {
        if b == b',' {
            if count == idx {
                return Some(&line[start..i]);
            }
            count += 1;
            start = i + 1;
        }
    }
    (count == idx).then(|| &line[start..])
}

fn split_into_chunks(body: &str, target_chunks: usize) -> Vec<&str> {
    if target_chunks <= 1 || body.is_empty() {
        return vec![body];
    }
    let approx_len = (body.len() / target_chunks).max(1);
    let mut chunks = Vec::with_capacity(target_chunks);
    let mut start = 0usize;
    let mut next_target = approx_len;
    for pos in memchr::memchr_iter(b'\n', body.as_bytes()) {
        if pos >= next_target {
            chunks.push(&body[start..=pos]);
            start = pos + 1;
            next_target = start + approx_len;
        }
    }
    if start < body.len() {
        chunks.push(&body[start..]);
    }
    chunks
}

fn filter_plain(body: &str, trip_idx: usize, bad: &HashSet<String>) -> (Vec<u8>, u64, u64) {
    let chunks = split_into_chunks(body, rayon::current_num_threads() * 4);

    let results: Vec<(Vec<u8>, u64, u64)> = chunks
        .par_iter()
        .map(|chunk| {
            let mut out = Vec::with_capacity(chunk.len());
            let mut kept = 0u64;
            let mut dropped = 0u64;
            for raw_line in chunk.split('\n') {
                let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
                if line.is_empty() {
                    continue;
                }
                let is_bad = nth_field(line, trip_idx).is_some_and(|v| bad.contains(v));
                if is_bad {
                    dropped += 1;
                } else {
                    out.extend_from_slice(line.as_bytes());
                    out.push(b'\n');
                    kept += 1;
                }
            }
            (out, kept, dropped)
        })
        .collect();

    let total_len: usize = results.iter().map(|(buf, _, _)| buf.len()).sum();
    let mut out = Vec::with_capacity(total_len);
    let mut kept = 0u64;
    let mut dropped = 0u64;
    for (buf, k, d) in results {
        out.extend_from_slice(&buf);
        kept += k;
        dropped += d;
    }
    (out, kept, dropped)
}

fn filter_quoted(
    body: &str,
    trip_idx: usize,
    bad: &HashSet<String>,
) -> anyhow::Result<(Vec<u8>, u64, u64)> {
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(false)
        .from_reader(body.as_bytes());
    let records: Vec<csv::StringRecord> = rdr.records().collect::<Result<_, _>>()?;
    let keep: Vec<bool> = records
        .par_iter()
        .map(|r| !bad.contains(&r[trip_idx]))
        .collect();

    let mut out = Vec::new();
    let mut wtr = csv::WriterBuilder::new()
        .terminator(csv::Terminator::Any(b'\n'))
        .from_writer(&mut out);
    let mut kept = 0u64;
    let mut dropped = 0u64;
    for (record, &keep) in records.iter().zip(keep.iter()) {
        if keep {
            wtr.write_record(record)?;
            kept += 1;
        } else {
            dropped += 1;
        }
    }
    wtr.flush()?;
    drop(wtr);
    Ok((out, kept, dropped))
}

fn filter_csv(name: &str, data: Vec<u8>, bad: &HashSet<String>) -> anyhow::Result<Vec<u8>> {
    let text = String::from_utf8(data)?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);

    let header_end = text.find('\n').unwrap_or(text.len());
    let header_line = text[..header_end]
        .strip_suffix('\r')
        .unwrap_or(&text[..header_end]);
    let body = &text[(header_end + 1).min(text.len())..];

    let trip_idx = header_line
        .split(',')
        .position(|h| h == "trip_id")
        .ok_or_else(|| anyhow::anyhow!("pas de colonne trip_id dans {name}"))?;

    let (mut out, kept, dropped) = if body.contains('"') {
        filter_quoted(body, trip_idx, bad)?
    } else {
        filter_plain(body, trip_idx, bad)
    };

    let mut result = Vec::with_capacity(header_line.len() + 1 + out.len());
    result.extend_from_slice(header_line.as_bytes());
    result.push(b'\n');
    result.append(&mut out);

    println!("{}: gardees {kept}, supprimees {dropped}", base_name(name));
    Ok(result)
}

fn last_departure_plain(body: &str, trip_idx: usize, dep_idx: usize) -> HashMap<&str, &str> {
    let chunks = split_into_chunks(body, rayon::current_num_threads() * 4);

    let per_chunk: Vec<HashMap<&str, &str>> = chunks
        .par_iter()
        .map(|chunk| {
            let mut map: HashMap<&str, &str> = HashMap::new();
            for raw_line in chunk.split('\n') {
                let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
                if line.is_empty() {
                    continue;
                }
                if let Some(trip) = nth_field(line, trip_idx) {
                    let dep = nth_field(line, dep_idx).unwrap_or("");
                    map.insert(trip, dep);
                }
            }
            map
        })
        .collect();

    let mut merged: HashMap<&str, &str> = HashMap::new();
    for chunk_map in per_chunk {
        merged.extend(chunk_map);
    }
    merged
}

fn last_departure_quoted(
    body: &str,
    trip_idx: usize,
    dep_idx: usize,
) -> anyhow::Result<HashMap<String, String>> {
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(false)
        .from_reader(body.as_bytes());
    let mut map: HashMap<String, String> = HashMap::new();
    for record in rdr.records() {
        let record = record?;
        let trip = record.get(trip_idx).unwrap_or("").to_string();
        let dep = record.get(dep_idx).unwrap_or("").to_string();
        map.insert(trip, dep);
    }
    Ok(map)
}

fn find_bad_trips(data: &[u8]) -> anyhow::Result<HashSet<String>> {
    let text = std::str::from_utf8(data)?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);

    let header_end = text.find('\n').unwrap_or(text.len());
    let header_line = text[..header_end]
        .strip_suffix('\r')
        .unwrap_or(&text[..header_end]);
    let body = &text[(header_end + 1).min(text.len())..];

    let mut trip_idx = None;
    let mut dep_idx = None;
    for (i, h) in header_line.split(',').enumerate() {
        match h {
            "trip_id" => trip_idx = Some(i),
            "departure_time" => dep_idx = Some(i),
            _ => {}
        }
    }
    let trip_idx =
        trip_idx.ok_or_else(|| anyhow::anyhow!("pas de colonne trip_id dans stop_times.txt"))?;
    let dep_idx = dep_idx
        .ok_or_else(|| anyhow::anyhow!("pas de colonne departure_time dans stop_times.txt"))?;

    let bad = if body.contains('"') {
        let last_dep = last_departure_quoted(body, trip_idx, dep_idx)?;
        last_dep
            .into_iter()
            .filter(|(_, dep)| dep.trim().is_empty())
            .map(|(trip, _)| trip)
            .collect()
    } else {
        let last_dep = last_departure_plain(body, trip_idx, dep_idx);
        last_dep
            .into_iter()
            .filter(|(_, dep)| dep.trim().is_empty())
            .map(|(trip, _)| trip.to_string())
            .collect()
    };
    Ok(bad)
}

fn parallel_deflate(data: &[u8]) -> anyhow::Result<(Vec<u8>, u32)> {
    let mut parz: ParCompress<Gzip, _> = ParCompressBuilder::new().from_writer(Vec::new());
    parz.write_all(data)?;
    let gz = parz
        .finish()
        .map_err(|e| anyhow::anyhow!("compression parallele echouee: {e}"))?;

    anyhow::ensure!(gz.len() >= 18, "sortie gzip invalide ({} octets)", gz.len());
    let crc32 = u32::from_le_bytes(gz[gz.len() - 8..gz.len() - 4].try_into().unwrap());
    let mut raw_deflate = gz;
    raw_deflate.truncate(raw_deflate.len() - 8);
    raw_deflate.drain(..10);
    Ok((raw_deflate, crc32))
}

fn build_single_entry_zip(
    name: &str,
    crc32: u32,
    raw_deflate: &[u8],
    uncompressed_size: u64,
) -> anyhow::Result<Vec<u8>> {
    let compressed_size = raw_deflate.len() as u64;
    anyhow::ensure!(
        uncompressed_size <= u32::MAX as u64 && compressed_size <= u32::MAX as u64,
        "{name}: trop volumineux pour un zip 32 bits"
    );
    let name_bytes = name.as_bytes();
    anyhow::ensure!(
        name_bytes.len() <= u16::MAX as usize,
        "{name}: nom de fichier trop long"
    );

    let mut out = Vec::with_capacity(30 + name_bytes.len() * 2 + raw_deflate.len() + 46 + 22);

    out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
    out.extend_from_slice(&20u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&8u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0x21u16.to_le_bytes());
    out.extend_from_slice(&crc32.to_le_bytes());
    out.extend_from_slice(&(compressed_size as u32).to_le_bytes());
    out.extend_from_slice(&(uncompressed_size as u32).to_le_bytes());
    out.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(name_bytes);
    out.extend_from_slice(raw_deflate);

    let central_dir_offset = out.len() as u32;
    out.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
    out.extend_from_slice(&20u16.to_le_bytes());
    out.extend_from_slice(&20u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&8u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0x21u16.to_le_bytes());
    out.extend_from_slice(&crc32.to_le_bytes());
    out.extend_from_slice(&(compressed_size as u32).to_le_bytes());
    out.extend_from_slice(&(uncompressed_size as u32).to_le_bytes());
    out.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(name_bytes);

    let central_dir_size = out.len() as u32 - central_dir_offset;
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&central_dir_size.to_le_bytes());
    out.extend_from_slice(&central_dir_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());

    Ok(out)
}

fn process_data(name: &str, data: Vec<u8>, bad: &HashSet<String>) -> anyhow::Result<Vec<u8>> {
    let filtered = filter_csv(name, data, bad)?;
    let uncompressed_size = filtered.len() as u64;
    let (raw_deflate, crc32) = parallel_deflate(&filtered)?;
    build_single_entry_zip(name, crc32, &raw_deflate, uncompressed_size)
}

fn process_target_file(src: &str, name: &str, bad: &HashSet<String>) -> anyhow::Result<Vec<u8>> {
    let file = File::open(src)?;
    let mut archive = ZipArchive::new(BufReader::new(file))?;
    let mut entry = archive.by_name(name)?;
    let mut data = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut data)?;
    drop(entry);

    process_data(name, data, bad)
}

fn output_path(src: &str) -> String {
    match src.strip_suffix(".zip") {
        Some(stem) => format!("{stem}.cleaned.zip"),
        None => format!("{src}.cleaned.zip"),
    }
}

fn main() -> anyhow::Result<()> {
    let src = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: gtfs-cleaner <fichier.zip>"))?;
    let output = output_path(&src);

    let file = File::open(&src)?;
    let mut archive = ZipArchive::new(BufReader::new(file))?;

    let names: Vec<String> = (0..archive.len())
        .map(|i| {
            archive
                .by_index(i)
                .and_then(|e| e.name().map(|n| n.into_owned()))
        })
        .collect::<Result<_, _>>()?;

    let stop_times_idx = names
        .iter()
        .position(|name| base_name(name) == "stop_times.txt")
        .ok_or_else(|| anyhow::anyhow!("stop_times.txt introuvable dans {src}"))?;
    let stop_times_name = names[stop_times_idx].clone();

    let mut stop_times_data = Vec::new();
    {
        let mut entry = archive.by_name(&stop_times_name)?;
        stop_times_data.reserve(entry.size() as usize);
        entry.read_to_end(&mut stop_times_data)?;
    }

    let bad = find_bad_trips(&stop_times_data)?;
    println!(
        "suppression de {} trips invalides (dernier stop_time sans departure_time)",
        bad.len()
    );

    let other_target_indices: Vec<(usize, String)> = names
        .iter()
        .enumerate()
        .filter(|(i, name)| *i != stop_times_idx && TARGET_FILES.contains(&base_name(name)))
        .map(|(i, name)| (i, name.clone()))
        .collect();

    let tmp = format!("{output}.tmp");
    let mut zout = ZipWriter::new(File::create(&tmp)?);

    std::thread::scope(|scope| -> anyhow::Result<()> {
        let bad_ref = &bad;
        let src_ref = &src;

        let stop_times_worker = scope.spawn(move || -> anyhow::Result<Vec<u8>> {
            process_data(&stop_times_name, stop_times_data, bad_ref)
        });

        let others_worker = scope.spawn(move || -> anyhow::Result<Vec<(usize, Vec<u8>)>> {
            other_target_indices
                .into_par_iter()
                .map(|(i, name)| -> anyhow::Result<_> {
                    let zipped = process_target_file(src_ref, &name, bad_ref)?;
                    Ok((i, zipped))
                })
                .collect()
        });

        for (i, name) in names.iter().enumerate() {
            if TARGET_FILES.contains(&base_name(name)) {
                continue;
            }
            let entry = archive.by_index(i)?;
            zout.raw_copy_file(entry)?;
            println!("{}: copied", base_name(name));
        }

        let stop_times_zipped = stop_times_worker
            .join()
            .expect("le thread stop_times.txt a paniqué")?;
        let mut processed = others_worker
            .join()
            .expect("le thread de traitement a paniqué")?;
        processed.push((stop_times_idx, stop_times_zipped));
        processed.sort_by_key(|(i, _)| *i);
        for (_, bytes) in processed {
            let mut sub = ZipArchive::new(Cursor::new(bytes))?;
            zout.raw_copy_file(sub.by_index(0)?)?;
        }

        Ok(())
    })?;

    zout.finish()?;
    std::fs::rename(&tmp, &output)?;
    println!("cleaned zip written to {output}");
    Ok(())
}
