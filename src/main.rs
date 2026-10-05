#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const API_BASE: &str = "https://benzinaoggi.app";

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Station {
    id: i64,
    nome: String,
    indirizzo: Option<String>,
    comune: Option<String>,
    provincia: Option<String>,
    bandiera: Option<String>,
    prezzo: f64,
    is_self: bool,
    lat: Option<f64>,
    lng: Option<f64>,
    distanza: Option<f64>,
    aggiornato_label: Option<String>,
}

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .user_agent("BenzinaOggiDesktop/1.0 (+https://benzinaoggi.app)")
        .timeout(std::time::Duration::from_secs(25))
        .build()
        .expect("http client")
}

/// Estrae nonce WordPress dalla homepage (FPI_HOME / FPI_DATA).
fn fetch_nonce(http: &reqwest::blocking::Client) -> Result<String, String> {
    let html = http
        .get(format!("{}/", API_BASE))
        .send()
        .map_err(|e| e.to_string())?
        .text()
        .map_err(|e| e.to_string())?;

    // "nonce":"...." oppure nonce: '....'
    let re = Regex::new(r#"nonce["']?\s*:\s*["']([a-zA-Z0-9]+)"#).unwrap();
    if let Some(cap) = re.captures(&html) {
        return Ok(cap[1].to_string());
    }
    Err("Nonce non trovato: apri benzinaoggi.app e verifica che il plugin sia attivo.".into())
}

fn geocode(http: &reqwest::blocking::Client, query: &str) -> Result<(f64, f64, String), String> {
    let q = format!("{}, Italia", query);
    let url = format!(
        "https://nominatim.openstreetmap.org/search?format=json&limit=1&countrycodes=it&q={}",
        urlencoding::encode(&q)
    );
    let arr: Value = http
        .get(&url)
        .header("Accept", "application/json")
        .send()
        .map_err(|e| e.to_string())?
        .json()
        .map_err(|e| e.to_string())?;

    let first = arr
        .as_array()
        .and_then(|a| a.first())
        .ok_or_else(|| "Località non trovata".to_string())?;

    let lat = first["lat"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .ok_or("lat non valida")?;
    let lng = first["lon"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .ok_or("lng non valida")?;
    let name = first["display_name"].as_str().unwrap_or(query).to_string();
    Ok((lat, lng, name))
}

fn search_api(
    http: &reqwest::blocking::Client,
    nonce: &str,
    fuel: &str,
    lat: f64,
    lng: f64,
    radius: i32,
    self_only: bool,
    nologo: bool,
    highway: bool,
) -> Result<Vec<Station>, String> {
    let mut form = vec![
        ("action".into(), "fpi_search".into()),
        ("nonce".into(), nonce.to_string()),
        ("fuel".into(), fuel.to_string()),
        ("provincia".into(), "".into()),
        ("comune".into(), "".into()),
        ("lat".into(), lat.to_string()),
        ("lng".into(), lng.to_string()),
        ("radius".into(), radius.to_string()),
        ("is_self".into(), if self_only { "1".into() } else { "".into() }),
    ];
    if nologo {
        form.push(("nologo".into(), "1".into()));
    }
    if highway {
        form.push(("highway".into(), "1".into()));
    }

    let url = format!("{}/wp-admin/admin-ajax.php", API_BASE);
    let res: Value = http
        .post(&url)
        .form(&form)
        .send()
        .map_err(|e| e.to_string())?
        .json()
        .map_err(|e| e.to_string())?;

    if res["success"] != true {
        return Err(res["data"]["message"]
            .as_str()
            .unwrap_or("Ricerca fallita")
            .to_string());
    }

    let mut out = Vec::new();
    if let Some(arr) = res["data"]["results"].as_array() {
        for r in arr {
            let prezzo = r["prezzo"].as_f64().or_else(|| {
                r["prezzo"].as_str().and_then(|s| s.replace(',', ".").parse().ok())
            }).unwrap_or(0.0);
            if prezzo <= 0.0 {
                continue;
            }
            out.push(Station {
                id: r["id"].as_i64().or_else(|| r["id_impianto"].as_i64()).unwrap_or(0),
                nome: r["nome"].as_str().or_else(|| r["nome_impianto"].as_str()).unwrap_or("Impianto").to_string(),
                indirizzo: r["indirizzo"].as_str().map(|s| s.to_string()),
                comune: r["comune"].as_str().map(|s| s.to_string()),
                provincia: r["provincia"].as_str().map(|s| s.to_string()),
                bandiera: r["bandiera"].as_str().map(|s| s.to_string()),
                prezzo,
                is_self: r["is_self"].as_bool().unwrap_or(false)
                    || r["is_self"].as_i64().unwrap_or(0) == 1,
                lat: r["lat"].as_f64(),
                lng: r["lng"].as_f64(),
                distanza: r["distanza"].as_f64().or_else(|| r["distanza_km"].as_f64()),
                aggiornato_label: r["aggiornato_label"].as_str().map(|s| s.to_string()),
            });
        }
    }
    out.sort_by(|a, b| a.prezzo.partial_cmp(&b.prezzo).unwrap_or(std::cmp::Ordering::Equal));
    Ok(out)
}

#[tauri::command]
fn search_stations(
    query: String,
    fuel: String,
    radius: i32,
    self_only: bool,
    nologo: bool,
    highway: bool,
) -> Result<Vec<Station>, String> {
    let http = client();
    let nonce = fetch_nonce(&http)?;
    let (lat, lng, _name) = geocode(&http, &query)?;
    search_api(&http, &nonce, &fuel, lat, lng, radius, self_only, nologo, highway)
}

#[tauri::command]
fn search_near_me(
    fuel: String,
    radius: i32,
    self_only: bool,
    nologo: bool,
    highway: bool,
) -> Result<Vec<Station>, String> {
    let http = client();
    // Fallback posizione via IP (approssimata) – su Windows l'utente può cercare per città
    let geo: Value = http
        .get("https://ipapi.co/json/")
        .send()
        .map_err(|e| e.to_string())?
        .json()
        .map_err(|e| e.to_string())?;

    let lat = geo["latitude"].as_f64().ok_or("Posizione IP non disponibile")?;
    let lng = geo["longitude"].as_f64().ok_or("Posizione IP non disponibile")?;
    let country = geo["country_code"].as_str().unwrap_or("");
    if country != "IT" {
        return Err("Posizione non in Italia: cerca una città dal campo testo.".into());
    }
    let nonce = fetch_nonce(&http)?;
    search_api(&http, &nonce, &fuel, lat, lng, radius.max(1).min(5), self_only, nologo, highway)
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![search_stations, search_near_me])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
