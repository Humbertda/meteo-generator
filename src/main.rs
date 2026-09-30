use serde::Deserialize;
use std::env;
use std::fs::{create_dir_all, File};
use std::io::Write;

#[derive(Deserialize, Debug)]
struct BanResponse {
    features: Vec<BanFeature>,
}

#[derive(Deserialize, Debug)]
struct BanFeature {
    geometry: BanGeometry,
    properties: BanProperties,
}

#[derive(Deserialize, Debug)]
struct BanGeometry {
    coordinates: [f64; 2],
}

#[derive(Deserialize, Debug)]
struct BanProperties {
    label: String,
}

#[derive(Deserialize)]
struct OpenMeteoResponse {
    daily: DailyData,
}

#[derive(Deserialize)]
struct DailyData {
    time: Vec<String>,
    temperature_2m_max: Vec<f64>,
    temperature_2m_min: Vec<f64>,
    precipitation_probability_max: Vec<Option<u8>>,
    weather_code: Vec<u8>,
}

fn wmo_to_emoji_and_str(code: u8) -> (&'static str, &'static str) {
    match code {
        0 => ("☀️", "Ciel dégagé"),
        1..=3 => ("⛅", "Partiellement nuageux"),
        45 | 48 => ("🌫️", "Brouillard"),
        51..=55 => ("🌦️", "Bruine"),
        61..=65 => ("🌧️", "Pluie"),
        71..=75 => ("❄️", "Neige"),
        80..=82 => ("🌧️", "Averses"),
        95..=99 => ("🌩️", "Orage"),
        _ => ("🌡️", "Variable"),
    }
}

/// Échappe les caractères spéciaux requis par le format iCalendar (RFC 5545)
fn escape_ical(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
}

async fn geocode_address(client: &reqwest::Client, query: &str) -> Option<(String, f64, f64)> {
    let url = "https://api-adresse.data.gouv.fr/search/";
    let resp = client
        .get(url)
        .query(&[("q", query), ("limit", "1")])
        .send()
        .await
        .ok()?;

    if resp.status().is_success() {
        let ban_resp = resp.json::<BanResponse>().await.ok()?;
        let feature = ban_resp.features.into_iter().next()?;
        let lon = feature.geometry.coordinates[0];
        let lat = feature.geometry.coordinates[1];
        let label = feature.properties.label;
        Some((label, lat, lon))
    } else {
        None
    }
}

async fn fetch_forecast(client: &reqwest::Client, lat: f64, lon: f64) -> Option<DailyData> {
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={lat}&longitude={lon}&daily=weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max&models=meteofrance_arome_france&forecast_days=5&timezone=Europe%2FParis"
    );

    let resp = client.get(&url).send().await.ok()?;
    if resp.status().is_success() {
        let data = resp.json::<OpenMeteoResponse>().await.ok()?;
        Some(data.daily)
    } else {
        None
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let locations_raw = env::var("WEATHER_LOCATIONS")
        .map_err(|_| "La variable d'environnement WEATHER_LOCATIONS n'est pas définie.")?;

    let address_queries: Vec<&str> = locations_raw
        .split(';')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();

    if address_queries.is_empty() {
        return Err("Aucune adresse valide trouvée dans WEATHER_LOCATIONS.".into());
    }

    // Ajout d'un User-Agent personnalisé pour respecter les API publiques
    let client = reqwest::Client::builder()
        .user_agent("weather_calendar/1.0")
        .build()?;

    let mut ics_content = String::from(
        "BEGIN:VCALENDAR\r\n\
         VERSION:2.0\r\n\
         PRODID:-//Meteo AROME Address Calendar//FR\r\n\
         CALSCALE:GREGORIAN\r\n\
         X-WR-CALNAME:Météo AROME (5 jours)\r\n\
         X-WR-TIMEZONE:Europe/Paris\r\n",
    );

    for query in address_queries {
        println!("Géocodage de : '{query}'...");
        if let Some((formatted_address, lat, lon)) = geocode_address(&client, query).await {
            println!("Trouvé : {formatted_address} (Lat: {lat}, Lon: {lon})");

            if let Some(daily) = fetch_forecast(&client, lat, lon).await {
                let days_count = daily.time.len().min(5);

                for i in 0..days_count {
                    let date_str = &daily.time[i];
                    let date_clean = date_str.replace('-', "");

                    let temp_max = daily.temperature_2m_max.get(i).copied().unwrap_or(0.0);
                    let temp_min = daily.temperature_2m_min.get(i).copied().unwrap_or(0.0);
                    let rain_prob = daily
                        .precipitation_probability_max
                        .get(i)
                        .and_then(|&opt| opt)
                        .unwrap_or(0);
                    let weather_code = daily.weather_code.get(i).copied().unwrap_or(0);
                    let (emoji, condition) = wmo_to_emoji_and_str(weather_code);

                    let escaped_addr = escape_ical(&formatted_address);

                    let summary = escape_ical(&format!(
                        "{emoji} {formatted_address} : {temp_min:.0}°C / {temp_max:.0}°C - {condition}"
                    ));

                    // Correction de la chaîne formatée avec interpolation directe
                    let description = escape_ical(&format!(
                        "Météo-France AROME (Maillage 1.3km)\\n\
                         Adresse: {formatted_address}\\n\
                         - Température Min: {temp_min:.1}°C\\n\
                         - Température Max: {temp_max:.1}°C\\n\
                         - Risque de pluie: {rain_prob}%\\n\
                         - Condition: {condition}"
                    ));

                    let addr_slug: String = formatted_address
                        .to_lowercase()
                        .chars()
                        .map(|c| if c.is_alphanumeric() { c } else { '-' })
                        .collect();

                    ics_content.push_str(&format!(
                        "BEGIN:VEVENT\r\n\
                         UID:weather-{addr_slug}-{date_clean}@meteo-arome\r\n\
                         DTSTAMP:20260101T000000Z\r\n\
                         DTSTART;VALUE=DATE:{date_clean}\r\n\
                         SUMMARY:{summary}\r\n\
                         DESCRIPTION:{description}\r\n\
                         LOCATION:{escaped_addr}\r\n\
                         STATUS:CONFIRMED\r\n\
                         END:VEVENT\r\n"
                    ));
                }
            }
        } else {
            eprintln!("Impossible de géocoder l'adresse : {query}");
        }
    }

    ics_content.push_str("END:VCALENDAR\r\n");

    create_dir_all("public")?;
    let mut file = File::create("public/weather.ics")?;
    file.write_all(ics_content.as_bytes())?;

    println!("Calendrier généré avec succès dans public/weather.ics !");
    Ok(())
}
