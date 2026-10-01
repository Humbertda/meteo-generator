use serde::Deserialize;
use std::env;
use std::fs::{create_dir_all, File};
use std::io::Write;
use chrono::Utc;

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

#[derive(Deserialize, Debug)]
struct OpenMeteoResponse {
    daily: DailyData,
    hourly: HourlyData,
}

#[derive(Deserialize, Debug)]
struct DailyData {
    time: Vec<String>,
    temperature_2m_max: Vec<f64>,
    temperature_2m_min: Vec<f64>,
    precipitation_probability_max: Vec<Option<u8>>,
    weather_code: Vec<u8>,
}

#[derive(Deserialize, Debug)]
struct HourlyData {
    time: Vec<String>,
    temperature_2m: Vec<f64>,
    precipitation_probability: Vec<Option<u8>>,
    weather_code: Vec<u8>,
}

struct ForecastData {
    daily: DailyData,
    hourly: HourlyData,
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
        95..=99 => ("🌩️️", "Orage"),
        _ => ("🌡️", "Variable"),
    }
}

fn escape_ical(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace("\r\n", "\\n")
        .replace('\n', "\\n")
}

async fn geocode_address(client: &reqwest::Client, query: &str) -> Option<(String, f64, f64)> {
    let url = "https://api-adresse.data.gouv.fr/search/";
    let resp = match client
        .get(url)
        .query(&[("q", query), ("limit", "1")])
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Erreur réseau lors du géocodage de '{query}': {e}");
            return None;
        }
    };

    if !resp.status().is_success() {
        eprintln!("Erreur HTTP BAN ({}) pour '{query}'", resp.status());
        return None;
    }

    match resp.json::<BanResponse>().await {
        Ok(ban_resp) => {
            let feature = ban_resp.features.into_iter().next()?;
            let lon = feature.geometry.coordinates[0];
            let lat = feature.geometry.coordinates[1];
            let label = feature.properties.label;
            Some((label, lat, lon))
        }
        Err(e) => {
            eprintln!("Erreur de désérialisation JSON BAN pour '{query}': {e}");
            None
        }
    }
}

async fn fetch_forecast(client: &reqwest::Client, lat: f64, lon: f64) -> Option<ForecastData> {
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={lat}&longitude={lon}\
        &daily=weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max\
        &hourly=temperature_2m,precipitation_probability,weather_code\
        &forecast_days=5&timezone=Europe%2FParis"
    );

    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Erreur réseau lors de la récupération météo : {e}");
            return None;
        }
    };

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        eprintln!("Erreur HTTP Open-Meteo ({status}) : {text}");
        return None;
    }

    match resp.json::<OpenMeteoResponse>().await {
        Ok(data) => Some(ForecastData {
            daily: data.daily,
            hourly: data.hourly,
        }),
        Err(e) => {
            eprintln!("Erreur de désérialisation JSON Open-Meteo : {e}");
            None
        }
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

    let client = reqwest::Client::builder()
        .user_agent("weather_calendar/1.0")
        .build()?;

    create_dir_all("public")?;

    for (idx, query) in address_queries.iter().enumerate() {
        let file_num = idx + 1;
        println!("\nTraitement lieu #{file_num} : '{query}'...");

        let mut ics_content = String::from(
            "BEGIN:VCALENDAR\r\n\
             VERSION:2.0\r\n\
             PRODID:-//Meteo Address Calendar//FR\r\n\
             CALSCALE:GREGORIAN\r\n\
             X-WR-CALNAME:Météo (5 jours)\r\n\
             X-WR-TIMEZONE:Europe/Paris\r\n",
        );

        if let Some((formatted_address, lat, lon)) = geocode_address(&client, query).await {
            println!("Trouvé : {formatted_address} (Lat: {lat}, Lon: {lon})");

            if let Some(forecast) = fetch_forecast(&client, lat, lon).await {
                let days_count = forecast.daily.time.len().min(5);

                for i in 0..days_count {
                    let date_str = &forecast.daily.time[i];
                    let date_clean = date_str.replace('-', "");

                    let temp_max = forecast.daily.temperature_2m_max.get(i).copied().unwrap_or(0.0);
                    let temp_min = forecast.daily.temperature_2m_min.get(i).copied().unwrap_or(0.0);
                    let rain_prob = forecast
                        .daily
                        .precipitation_probability_max
                        .get(i)
                        .and_then(|&opt| opt)
                        .unwrap_or(0);
                    let weather_code = forecast.daily.weather_code.get(i).copied().unwrap_or(0);
                    let (emoji, condition) = wmo_to_emoji_and_str(weather_code);

                    // Extraction du détail horaire (toutes les 3 heures : 00h, 03h, 06h...)
                    let start_h = i * 24;
                    let end_h = (start_h + 24).min(forecast.hourly.time.len());
                    let mut hourly_lines = Vec::new();

                    for h in (start_h..end_h).step_by(3) {
                        if let Some(time_str) = forecast.hourly.time.get(h) {
                            let hour_label = time_str
                                .split('T')
                                .nth(1)
                                .and_then(|t| t.get(..5))
                                .unwrap_or("");
                            let h_temp = forecast.hourly.temperature_2m.get(h).copied().unwrap_or(0.0);
                            let h_rain = forecast
                                .hourly
                                .precipitation_probability
                                .get(h)
                                .and_then(|&opt| opt)
                                .unwrap_or(0);
                            let h_code = forecast.hourly.weather_code.get(h).copied().unwrap_or(0);
                            let (h_emoji, _) = wmo_to_emoji_and_str(h_code);

                            hourly_lines.push(format!(
                                "• {hour_label} : {h_temp:.1}°C | Pluie {h_rain}% {h_emoji}"
                            ));
                        }
                    }

                    let hourly_text = hourly_lines.join("\n");
                    let escaped_addr = escape_ical(&formatted_address);

                    let summary = escape_ical(&format!(
                        "{emoji} {temp_min:.0}°C / {temp_max:.0}°C - {condition} - {formatted_address}"
                    ));

                    let description = escape_ical(&format!(
    "Météo (5 jours)\n\
     Adresse: {formatted_address}\n\
     - Température Min: {temp_min:.1}°C\n\
     - Température Max: {temp_max:.1}°C\n\
     - Risque max pluie: {rain_prob}%\n\
     - Condition: {condition}\n\n\
     Détail horaire :\n\
     {hourly_text}"
));


                    let addr_slug: String = formatted_address
                        .to_lowercase()
                        .chars()
                        .map(|c| if c.is_alphanumeric() { c } else { '-' })
                        .collect();

                    let dtstamp = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
                    ics_content.push_str(&format!(
                       "BEGIN:VEVENT\r\n\
                         UID:weather-{addr_slug}-{date_clean}@meteo-app\r\n\
                         DTSTAMP:{dtstamp}\r\n\
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

        ics_content.push_str("END:VCALENDAR\r\n");

        let file_path = format!("public/{file_num}.ics");
        let mut file = File::create(&file_path)?;
        file.write_all(ics_content.as_bytes())?;
        println!("Fichier généré : {file_path}");
    }

    println!("\nTous les fichiers ont été générés dans le dossier public/ !");
    Ok(())
}
