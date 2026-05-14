use eframe::egui::{self, Color32, Pos2, Rect, Shape, Stroke};
use egui::{ScrollArea, Vec2, ViewportBuilder};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};
use tokio::runtime::Runtime;

const EPS: f64 = 1e-6;
const DEFAULT_BTC_CAGR_4Y: f64 = 0.0;

// ---------------- DISPLAY MODE ----------------

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize, Debug)]
enum DisplayMode {
    BTC,
    SATS,
}

impl Default for DisplayMode {
    fn default() -> Self {
        DisplayMode::BTC
    }
}

impl std::fmt::Display for DisplayMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DisplayMode::BTC => write!(f, "BTC"),
            DisplayMode::SATS => write!(f, "SATS"),
        }
    }
}

// ---------------- APP ----------------

#[derive(Serialize, Deserialize)]
struct ExpenseApp {
    total_budget: f64,
    expenses: Vec<(String, f64)>,

    #[serde(skip)]
    btc_prices: HashMap<String, f64>,

    #[serde(skip)]
    selected_currency: String,

    #[serde(skip)]
    supported_currencies: Vec<String>,

    #[serde(skip)]
    rt: Option<Runtime>,

    #[serde(skip)]
    btc_rx: Option<Receiver<HashMap<String, f64>>>,

    #[serde(skip)]
    inflation_rates: HashMap<String, f64>,

    #[serde(skip)]
    btc_cagr_4y: f64,

    #[serde(skip)]
    macro_rx: Option<Receiver<(HashMap<String, f64>, f64)>>,

    #[serde(skip)]
    last_macro_refresh: Option<Instant>,

    #[serde(skip)]
    macro_loading: bool,

    #[serde(default)]
    display_mode: DisplayMode,
}

impl ExpenseApp {
    fn new() -> Self {
        Self {
            total_budget: 1000.0,
            expenses: vec![
                ("Rent".to_string(), 200.0),
                ("Food".to_string(), 150.0),
            ],
            btc_prices: HashMap::new(),
            selected_currency: "usd".to_string(),
            supported_currencies: vec![
                "usd",
                "gbp",
                "eur",
                "jpy",
                "cad",
                "aud",
                "chf",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            rt: Some(Runtime::new().unwrap()),
            btc_rx: None,
            inflation_rates: HashMap::new(),
            btc_cagr_4y: DEFAULT_BTC_CAGR_4Y,
            macro_rx: None,
            last_macro_refresh: None,
            macro_loading: false,
            display_mode: DisplayMode::BTC,
        }
    }

    // ---------------- INFLATION ----------------

    fn inflation_rate_for(&self, currency: &str) -> f64 {
        *self.inflation_rates.get(currency).unwrap_or(&0.03)
    }

    // ---------------- BTC PRICE FETCH ----------------

    fn fetch_btc(&mut self) {
        let (tx, rx) = mpsc::channel();

        let rt = self.rt.as_ref().unwrap();

        rt.spawn(async move {
            let client = Client::new();

            let mut prices = HashMap::new();

            let url =
                "https://api.coingecko.com/api/v3/simple/price?ids=bitcoin&vs_currencies=usd,gbp,eur,jpy,cad,aud,chf";

            if let Ok(resp) = client
                .get(url)
                .header("User-Agent", "satsbudget")
                .send()
                .await
            {
                if let Ok(text) = resp.text().await {
                    if let Ok(json) =
                        serde_json::from_str::<serde_json::Value>(&text)
                    {
                        if let Some(btc) = json.get("bitcoin") {
                            for key in [
                                "usd",
                                "gbp",
                                "eur",
                                "jpy",
                                "cad",
                                "aud",
                                "chf",
                            ] {
                                if let Some(v) =
                                    btc.get(key).and_then(|v| v.as_f64())
                                {
                                    prices.insert(key.to_string(), v);
                                }
                            }
                        }
                    }
                }
            }

            let _ = tx.send(prices);
        });

        self.btc_rx = Some(rx);
    }

    // ---------------- MACRO DATA FETCH ----------------

    fn fetch_macro_data(&mut self) {
        let (tx, rx) = mpsc::channel();

        let rt = self.rt.as_ref().unwrap();

        rt.spawn(async move {
            let client = Client::new();

            let mut infl = HashMap::new();

            let countries = vec![
                ("usd", "US"),
                ("gbp", "GB"),
                ("eur", "EU"),
                ("jpy", "JP"),
                ("cad", "CA"),
                ("aud", "AU"),
                ("chf", "CH"),
            ];

            // ---------------- BTC CAGR (BINANCE) ----------------

            let mut btc_cagr = DEFAULT_BTC_CAGR_4Y;

            let btc_url =
                "https://api.binance.com/api/v3/klines?symbol=BTCUSDT&interval=1M&limit=48";

            if let Ok(resp) = client.get(btc_url).send().await {
                if let Ok(text) = resp.text().await {

                    println!("BTC API RESPONSE: {}", text);

                    if let Ok(json) =
                        serde_json::from_str::<serde_json::Value>(&text)
                    {
                        if let Some(arr) = json.as_array() {
                            if arr.len() >= 2 {

                                let first = arr
                                    .first()
                                    .and_then(|v| v.get(4))
                                    .and_then(|v| v.as_str())
                                    .and_then(|v| v.parse::<f64>().ok());

                                let last = arr
                                    .last()
                                    .and_then(|v| v.get(4))
                                    .and_then(|v| v.as_str())
                                    .and_then(|v| v.parse::<f64>().ok());

                                if let (Some(first), Some(last)) =
                                    (first, last)
                                {
                                    if first > 0.0 {

                                        btc_cagr =
                                            (last / first)
                                                .powf(1.0 / 4.0)
                                                - 1.0;

                                        println!(
                                            "LIVE BTC CAGR: {:.2}%",
                                            btc_cagr * 100.0
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ---------------- LIVE INFLATION ----------------

            for (currency, country) in countries {

                let url = format!(
                    "https://api.worldbank.org/v2/country/{}/indicator/FP.CPI.TOTL.ZG?format=json",
                    country
                );

                if let Ok(resp) = client.get(&url).send().await {

                    if let Ok(text) = resp.text().await {

                        if let Ok(json) =
                            serde_json::from_str::<serde_json::Value>(&text)
                        {
                            if let Some(arr) =
                                json.get(1).and_then(|v| v.as_array())
                            {
                                let mut vals = vec![];

                                for item in arr.iter().take(4) {

                                    if let Some(v) =
                                        item.get("value")
                                            .and_then(|v| v.as_f64())
                                    {
                                        vals.push(v);
                                    }
                                }

                                if !vals.is_empty() {

                                    let compounded =
										vals.iter()
											.fold(1.0, |acc, v| acc * (1.0 + v / 100.0));

									let avg =
										compounded.powf(1.0 / vals.len() as f64) - 1.0;

                                    infl.insert(currency.to_string(), avg);
                                }
                            }
                        }
                    }
                }
            }

            if infl.is_empty() {
                println!("Macro fetch failed.");
            }

            let _ = tx.send((infl, btc_cagr));
        });

        self.macro_rx = Some(rx);
    }

    fn btc_price(&self) -> f64 {
        *self
            .btc_prices
            .get(&self.selected_currency)
            .unwrap_or(&0.0)
    }

    fn btc_value(&self, fiat: f64) -> f64 {
        let p = self.btc_price();

        if p <= EPS {
            return 0.0;
        }

        fiat / p
    }

    fn fmt_hard(&self, btc: f64) -> String {
        match self.display_mode {
            DisplayMode::BTC => format!("{:.8} BTC", btc),
            DisplayMode::SATS => {
                format!("{} SATS", (btc * 100_000_000.0) as u64)
            }
        }
    }

    // ---------------- PIE CHART ----------------

    fn draw_pie(&self, painter: &egui::Painter, rect: Rect) {

        let center = rect.center();

        let radius = rect.height().min(rect.width()) * 0.28;

        let spent: f64 =
            self.expenses.iter().map(|(_, v)| v).sum();

        let remaining =
            (self.total_budget - spent).max(0.0);

        let mut data: Vec<(String, f64)> =
            self.expenses.clone();

        if remaining > EPS {
            data.push(("Remaining".to_string(), remaining));
        }

        let total: f64 =
            data.iter().map(|(_, v)| v).sum();

        if total <= EPS {
            return;
        }

        let colors = [
            Color32::LIGHT_RED,
            Color32::LIGHT_GREEN,
            Color32::LIGHT_BLUE,
            Color32::YELLOW,
            Color32::RED,
            Color32::BLUE,
        ];

        let mut start = 0.0;

        for (i, (label, val)) in data.iter().enumerate() {

            if *val <= EPS {
                continue;
            }

            let pct = val / total;

            let sweep =
                pct as f32 * std::f32::consts::TAU;

            let color = if label == "Remaining" {
                Color32::from_rgb(255, 165, 0)
            } else {
                colors[i % colors.len()]
            };

            let mut pts = vec![center];

            for j in 0..=40 {

                let t =
                    start + sweep * j as f32 / 40.0;

                pts.push(Pos2::new(
                    center.x + radius * t.cos(),
                    center.y + radius * t.sin(),
                ));
            }

            painter.add(
                Shape::convex_polygon(
                    pts,
                    color,
                    Stroke::NONE,
                )
            );

            let mid = start + sweep / 2.0;

            let inside = pct > 0.10;

            if inside {

                let label_pos = Pos2::new(
                    center.x + (radius * 0.55) * mid.cos(),
                    center.y + (radius * 0.55) * mid.sin(),
                );

                painter.text(
                    label_pos,
                    egui::Align2::CENTER_CENTER,
                    format!(
                        "{} ({:.0}%)",
                        label,
                        pct * 100.0
                    ),
                    egui::FontId::proportional(12.0),
                    Color32::BLACK,
                );

            } else {

                let edge = Pos2::new(
                    center.x + radius * mid.cos(),
                    center.y + radius * mid.sin(),
                );

                let outer = Pos2::new(
                    center.x + (radius + 40.0) * mid.cos(),
                    center.y + (radius + 40.0) * mid.sin(),
                );

                painter.line_segment(
                    [edge, outer],
                    Stroke::new(1.0, Color32::WHITE),
                );

                painter.text(
                    outer,
                    egui::Align2::CENTER_CENTER,
                    format!(
                        "{} ({:.0}%)",
                        label,
                        pct * 100.0
                    ),
                    egui::FontId::proportional(12.0),
                    Color32::WHITE,
                );
            }

            start += sweep;
        }
    }
}

// ---------------- UI ----------------

impl eframe::App for ExpenseApp {

    fn update(
        &mut self,
        ctx: &egui::Context,
        _frame: &mut eframe::Frame,
    ) {

        ctx.set_visuals(egui::Visuals::dark());

        // ---------------- BTC PRICE FETCH ----------------

        if self.btc_rx.is_none()
            && self.btc_prices.is_empty()
        {
            self.fetch_btc();
        }

        // ---------------- MACRO REFRESH CONTROL ----------------

        let should_refresh = match self.last_macro_refresh {
            None => true,
            Some(t) => t.elapsed() > Duration::from_secs(3600),
        };

        if should_refresh
            && self.macro_rx.is_none()
            && !self.macro_loading
        {
            self.macro_loading = true;
            self.fetch_macro_data();
        }

        // ---------------- BTC PRICE RECEIVE ----------------

        if let Some(rx) = &self.btc_rx {

            if let Ok(v) = rx.try_recv() {
                self.btc_prices = v;
                self.btc_rx = None;
            }
        }

        // ---------------- MACRO RECEIVE ----------------

        if let Some(rx) = &self.macro_rx {

            if let Ok((infl, btc_cagr)) =
                rx.try_recv()
            {
                self.inflation_rates = infl;
                self.btc_cagr_4y = btc_cagr;

                self.macro_rx = None;

                self.last_macro_refresh =
                    Some(Instant::now());

                self.macro_loading = false;
            }
        }

        // ---------------- UI ----------------

        egui::CentralPanel::default().show(
            ctx,
            |ui| {

                ScrollArea::vertical()
                    .auto_shrink([false; 2])
                    .show(ui, |ui| {

                        ui.horizontal(|ui| {

                            ui.heading(format!(
                                "BTC Price: {} {}",
                                self.btc_price() as u64,
                                self.selected_currency
                                    .to_uppercase()
                            ));

                            ui.with_layout(
                                egui::Layout::right_to_left(
                                    egui::Align::TOP
                                ),
                                |ui| {

                                    ui.selectable_value(
                                        &mut self.display_mode,
                                        DisplayMode::BTC,
                                        "BTC",
                                    );

                                    ui.selectable_value(
                                        &mut self.display_mode,
                                        DisplayMode::SATS,
                                        "SATS",
                                    );
                                },
                            );
                        });

                        ui.separator();

                        ui.horizontal(|ui| {

                            ui.label("Currency:");

                            for c in
                                &self.supported_currencies
                            {
                                ui.selectable_value(
                                    &mut self.selected_currency,
                                    c.clone(),
                                    c.to_uppercase(),
                                );
                            }

                            ui.label("Budget:");

                            ui.add(
                                egui::DragValue::new(
                                    &mut self.total_budget
                                )
                            );
                        });

                        for (name, val)
                            in &mut self.expenses
                        {
                            ui.horizontal(|ui| {

                                ui.text_edit_singleline(
                                    name
                                );

                                ui.add(
                                    egui::DragValue::new(val)
                                );
                            });
                        }

                        ui.horizontal(|ui| {

                            if ui.button("Add").clicked() {
                                self.expenses.push((
                                    "New".to_string(),
                                    0.0,
                                ));
                            }

                            if ui.button("Delete").clicked() {
                                self.expenses.pop();
                            }
                        });

                        ui.separator();

                        ui.columns(2, |cols| {

                            cols[0].vertical(|ui| {

                                let spent: f64 =
                                    self.expenses
                                        .iter()
                                        .map(|(_, v)| v)
                                        .sum();

                                let remaining =
                                    (self.total_budget - spent)
                                        .max(0.0);

                                ui.group(|ui| {

                                    ui.heading(
                                        "Budget Summary"
                                    );

                                    ui.separator();

                                    ui.columns(3, |c| {

                                        c[0].heading("Expense");

                                        c[1].heading(format!(
                                            "{} Value",
                                            self.selected_currency
                                                .to_uppercase()
                                        ));

                                        c[2].heading(
                                            "BTC Value"
                                        );
                                    });

                                    ui.separator();

                                    for (name, val)
                                        in &self.expenses
                                    {
                                        ui.columns(3, |c| {

                                            c[0].label(name);

                                            c[1].label(
                                                format!(
                                                    "{:.2} {}",
                                                    val,
                                                    self.selected_currency
                                                        .to_uppercase()
                                                )
                                            );

                                            c[2].label(
                                                self.fmt_hard(
                                                    self.btc_value(*val)
                                                )
                                            );
                                        });
                                    }

                                    ui.separator();

                                    ui.columns(3, |c| {

                                        c[0].label("Spent");

                                        c[1].label(
                                            format!(
                                                "{:.2} {}",
                                                spent,
                                                self.selected_currency
                                                    .to_uppercase()
                                            )
                                        );

                                        c[2].label(
                                            self.fmt_hard(
                                                self.btc_value(spent)
                                            )
                                        );
                                    });

                                    ui.columns(3, |c| {

                                        c[0].label(
                                            "Remaining"
                                        );

                                        c[1].label(
                                            format!(
                                                "{:.2} {}",
                                                remaining,
                                                self.selected_currency
                                                    .to_uppercase()
                                            )
                                        );

                                        c[2].label(
                                            self.fmt_hard(
                                                self.btc_value(
                                                    remaining
                                                )
                                            )
                                        );
                                    });
                                });

                                ui.add_space(12.0);

                                ui.label(
                                    egui::RichText::new(
                                        format!(
                                            "YOU HAVE {} IN HARD MONEY REMAINING!",
                                            self.fmt_hard(
                                                self.btc_value(
                                                    remaining
                                                )
                                            )
                                        )
                                    )
                                    .color(
                                        Color32::LIGHT_GREEN
                                    )
                                    .size(18.0),
                                );

                                ui.add_space(6.0);

                                ui.label(
                                    "Below is a chart showing how inflation will eat that money's purchasing power vs how Bitcoin will preserve it."
                                );

                                ui.add_space(12.0);

                                ui.group(|ui| {

                                    let inflation =
                                        self.inflation_rate_for(
                                            &self.selected_currency
                                        );

                                    ui.heading(
                                        "Purchasing Power Projection"
                                    );

                                    ui.separator();

                                    ui.label(format!(
                                        "Inflation: {:.2}% | BTC CAGR: {:.2}%",
                                        inflation * 100.0,
                                        self.btc_cagr_4y * 100.0
                                    ));

                                    ui.separator();

                                    ui.columns(3, |c| {

                                        c[0].heading(
                                            "Time Scale"
                                        );

                                        c[1].heading(
                                            format!(
                                                "{} Purchasing Power",
                                                self.selected_currency
                                                    .to_uppercase()
                                            )
                                        );

                                        c[2].heading(
                                            "BTC Purchasing Power"
                                        );
                                    });

                                    ui.separator();

                                    for y in
                                        [1, 4, 5, 10, 15, 20, 25, 30]
                                    {

                                        let fiat =
                                            remaining
                                                / (1.0 + inflation)
                                                    .powi(y);

                                        let btc_equiv =
                                            remaining
                                                * (1.0
                                                    + self.btc_cagr_4y)
                                                    .powi(y);

                                        ui.columns(3, |c| {

                                            c[0].label(
                                                format!(
                                                    "{} Year{}",
                                                    y,
                                                    if y > 1 {
                                                        "s"
                                                    } else {
                                                        ""
                                                    }
                                                )
                                            );

                                            c[1].label(
                                                format!(
                                                    "{:.2} {}",
                                                    fiat,
                                                    self.selected_currency
                                                        .to_uppercase()
                                                )
                                            );

                                            c[2].label(
                                                format!(
                                                    "{:.2} {}",
                                                    btc_equiv,
                                                    self.selected_currency
                                                        .to_uppercase()
                                                )
                                            );
										});
									}
									
									ui.add_space(10.0);

									// ---------------- DISCLAIMER ----------------
									ui.separator();

									ui.label(
										egui::RichText::new(
											"*Data sources: Inflation rates are derived from the World Bank CPI (Consumer Price Index) dataset using the most recent available annual data, averaged over the last 4 reported years and converted into a compounded annual growth rate. BTC CAGR is calculated from Binance monthly BTC/USDT closing prices over the last 48 months (approx. 4 years), using a compounded annual growth formula based on first vs last observed monthly close."
										)
										.color(Color32::GRAY)
										.size(11.0),
									);
                                
                                });
                            });

                            cols[1].vertical(|ui| {

                                let size =
                                    ui.available_size();

                                let desired =
                                    Vec2::new(
                                        size.x.max(400.0),
                                        size.y.max(500.0),
                                    );

                                let (_id, rect) =
                                    ui.allocate_space(
                                        desired
                                    );

                                self.draw_pie(
                                    ui.painter(),
                                    rect,
                                );
                            });
                        });
                    });
            },
        );
    }
}

fn main() -> eframe::Result<()> {

    let app = ExpenseApp::new();

    let options = eframe::NativeOptions {

        viewport: ViewportBuilder::default()
            .with_inner_size([1100.0, 750.0]),

        ..Default::default()
    };

    eframe::run_native(
        "SatsBudget",
        options,
        Box::new(|_| Ok(Box::new(app))),
    )
}