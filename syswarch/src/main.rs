
use std::fmt;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use chrono::Local;
use sysinfo::System;

// ─────────────────────────────────────────────────────────────
// ÉTAPE 1 — Modélisation des données
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct CpuInfo {
    usage_percent: f32,
    num_cores: usize,
}

impl fmt::Display for CpuInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "CPU : {:.1}%  ({} cœurs)",
            self.usage_percent, self.num_cores
        )
    }
}

// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct MemInfo {
    total_mb: u64,
    used_mb: u64,
    free_mb: u64,
}

impl fmt::Display for MemInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let pct = if self.total_mb > 0 {
            self.used_mb as f64 / self.total_mb as f64 * 100.0
        } else {
            0.0
        };
        write!(
            f,
            "RAM : {}/{} Mo utilisés  ({:.1}%)",
            self.used_mb, self.total_mb, pct
        )
    }
}

// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct ProcessInfo {
    pid: u32,
    name: String,
    cpu_usage: f32,
    mem_mb: u64,
}

impl fmt::Display for ProcessInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:<8} {:<25} CPU:{:>6.1}%  MEM:{:>6} Mo",
            self.pid, self.name, self.cpu_usage, self.mem_mb
        )
    }
}

// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct SystemSnapshot {
    timestamp: String,
    cpu: CpuInfo,
    mem: MemInfo,
    processes: Vec<ProcessInfo>,
}

impl fmt::Display for SystemSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "=== SysWatch — {} ===", self.timestamp)?;
        writeln!(f, "{}", self.cpu)?;
        writeln!(f, "{}", self.mem)?;
        writeln!(f, "--- Top processus ---")?;
        for p in &self.processes {
            writeln!(f, "  {}", p)?;
        }
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────
// ÉTAPE 2 — Collecte réelle avec gestion d'erreurs
// ─────────────────────────────────────────────────────────────

#[derive(Debug)]
enum SysWatchError {
    CollectionError(String),
}

impl fmt::Display for SysWatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SysWatchError::CollectionError(msg) => write!(f, "Erreur de collecte : {}", msg),
        }
    }
}

impl std::error::Error for SysWatchError {}

fn collect_snapshot() -> Result<SystemSnapshot, SysWatchError> {
    let mut sys = System::new_all();

    // Premier refresh pour initialiser les compteurs CPU
    sys.refresh_all();
    // Pause nécessaire : sysinfo calcule l'usage CPU en différentiel
    thread::sleep(Duration::from_millis(200));
    sys.refresh_all();

    // CPU
    let cpu_usage = sys.global_cpu_usage();
    let num_cores = sys.cpus().len();
    if num_cores == 0 {
        return Err(SysWatchError::CollectionError(
            "Impossible de lire les informations CPU".to_string(),
        ));
    }
    let cpu = CpuInfo {
        usage_percent: cpu_usage,
        num_cores,
    };

    // Mémoire (sysinfo renvoie des octets)
    let total_mb = sys.total_memory() / 1_048_576;
    let used_mb = sys.used_memory() / 1_048_576;
    let free_mb = sys.free_memory() / 1_048_576;
    let mem = MemInfo {
        total_mb,
        used_mb,
        free_mb,
    };

    // Processus : top 5 CPU
    let mut processes: Vec<ProcessInfo> = sys
        .processes()
        .values()
        .map(|p| ProcessInfo {
            pid: p.pid().as_u32(),
            name: p.name().to_string_lossy().to_string(),
            cpu_usage: p.cpu_usage(),
            mem_mb: p.memory() / 1_048_576,
        })
        .collect();

    processes.sort_by(|a, b| b.cpu_usage.partial_cmp(&a.cpu_usage).unwrap_or(std::cmp::Ordering::Equal));
    processes.truncate(5);

    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

    Ok(SystemSnapshot {
        timestamp,
        cpu,
        mem,
        processes,
    })
}

// ─────────────────────────────────────────────────────────────
// ÉTAPE 3 — Formatage des réponses réseau
// ─────────────────────────────────────────────────────────────

/// Construit une barre ASCII proportionnelle sur `width` caractères.
fn ascii_bar(percent: f64, width: usize) -> String {
    let filled = ((percent / 100.0) * width as f64).round() as usize;
    let filled = filled.min(width);
    let empty = width - filled;
    format!("[{}{}] {:.1}%", "#".repeat(filled), ".".repeat(empty), percent)
}

fn format_response(snapshot: &SystemSnapshot, command: &str) -> String {
    match command.trim().to_lowercase().as_str() {
        "cpu" => {
            let bar = ascii_bar(snapshot.cpu.usage_percent as f64, 30);
            format!(
                "=== CPU ===\nCœurs : {}\nUsage  : {}\nHorodatage : {}\n",
                snapshot.cpu.num_cores, bar, snapshot.timestamp
            )
        }

        "mem" => {
            let pct = if snapshot.mem.total_mb > 0 {
                snapshot.mem.used_mb as f64 / snapshot.mem.total_mb as f64 * 100.0
            } else {
                0.0
            };
            let bar = ascii_bar(pct, 30);
            format!(
                "=== MÉMOIRE ===\nTotal  : {} Mo\nUtilisé: {} Mo\nLibre  : {} Mo\nUsage  : {}\n",
                snapshot.mem.total_mb,
                snapshot.mem.used_mb,
                snapshot.mem.free_mb,
                bar
            )
        }

        "ps" => {
            let header = format!(
                "=== PROCESSUS (top 5 CPU) — {} ===\n{:<8} {:<25} {:>8}  {:>8}\n{}\n",
                snapshot.timestamp,
                "PID",
                "NOM",
                "CPU%",
                "MEM Mo",
                "-".repeat(55)
            );
            let rows: String = snapshot
                .processes
                .iter()
                .map(|p| format!("{}\n", p))
                .collect();
            format!("{}{}", header, rows)
        }

        "all" => {
            format!(
                "{}\n{}\n{}",
                format_response(snapshot, "cpu"),
                format_response(snapshot, "mem"),
                format_response(snapshot, "ps")
            )
        }

        "help" => {
            "Commandes disponibles :\n  cpu   — usage CPU\n  mem   — état de la RAM\n  ps    — top 5 processus\n  all   — tout afficher\n  help  — cette aide\n  quit  — fermer la connexion\n".to_string()
        }

        "quit" => "Au revoir !\n".to_string(),

        _ => format!("Commande inconnue : '{}'\nTapez 'help' pour la liste des commandes.\n", command.trim()),
    }
}

// ─────────────────────────────────────────────────────────────
// ÉTAPE 5 — Journalisation fichier (Bonus)
// ─────────────────────────────────────────────────────────────

fn log_event(msg: &str) {
    let ts = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .open("syswatch.log")
    {
        let _ = writeln!(f, "[{}] {}", ts, msg);
    }
}

// ─────────────────────────────────────────────────────────────
// ÉTAPE 4 — Serveur TCP multi-threadé
// ─────────────────────────────────────────────────────────────

fn handle_client(stream: TcpStream, shared: Arc<Mutex<SystemSnapshot>>) {
    let peer = stream
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| "inconnu".to_string());

    log_event(&format!("Connexion acceptée depuis {}", peer));
    println!("[INFO] Client connecté : {}", peer);

    let mut writer = stream.try_clone().expect("Impossible de cloner le flux TCP");
    let reader = BufReader::new(stream);

    // Message de bienvenue
    let _ = writeln!(writer, "Bienvenue sur SysWatch ! Tapez 'help' pour les commandes.");

    for line in reader.lines() {
        match line {
            Ok(cmd) => {
                let cmd = cmd.trim().to_lowercase();
                log_event(&format!("Commande reçue de {} : '{}'", peer, cmd));
                println!("[CMD] {} -> '{}'", peer, cmd);

                if cmd == "quit" {
                    let _ = write!(writer, "{}", format_response(&SystemSnapshot {
                        timestamp: String::new(),
                        cpu: CpuInfo { usage_percent: 0.0, num_cores: 0 },
                        mem: MemInfo { total_mb: 0, used_mb: 0, free_mb: 0 },
                        processes: vec![],
                    }, "quit"));
                    break;
                }

                // Lire le snapshot protégé
                let snapshot = {
                    let lock = shared.lock().expect("Mutex empoisonné");
                    lock.clone()
                };

                let response = format_response(&snapshot, &cmd);
                if write!(writer, "{}", response).is_err() {
                    break;
                }
                // Séparateur de fin de réponse (facilite la lecture côté client)
                let _ = writeln!(writer, "---");
            }
            Err(_) => break,
        }
    }

    log_event(&format!("Connexion fermée : {}", peer));
    println!("[INFO] Client déconnecté : {}", peer);
}

fn main() {
    println!("=== SysWatch démarrage ===");

    // Collecte initiale
    let initial = collect_snapshot().unwrap_or_else(|e| {
        eprintln!("Erreur lors de la collecte initiale : {}", e);
        std::process::exit(1);
    });

    let shared = Arc::new(Mutex::new(initial));

    // Thread de rafraîchissement toutes les 5 secondes
    let shared_refresh = Arc::clone(&shared);
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(5));
        match collect_snapshot() {
            Ok(snap) => {
                let mut lock = shared_refresh.lock().expect("Mutex empoisonné");
                *lock = snap;
                println!("[REFRESH] Snapshot mis à jour.");
            }
            Err(e) => eprintln!("[ERREUR] Rafraîchissement : {}", e),
        }
    });

    // Démarrage du serveur TCP
    let addr = "127.0.0.1:7878";
    let listener = TcpListener::bind(addr).unwrap_or_else(|e| {
        eprintln!("Impossible de démarrer le serveur sur {} : {}", addr, e);
        std::process::exit(1);
    });

    log_event(&format!("Serveur démarré sur {}", addr));
    println!("Serveur en écoute sur {}  (Ctrl+C pour arrêter)", addr);

    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let shared_client = Arc::clone(&shared);
                thread::spawn(move || handle_client(s, shared_client));
            }
            Err(e) => eprintln!("[ERREUR] Connexion entrante : {}", e),
        }
    }
}