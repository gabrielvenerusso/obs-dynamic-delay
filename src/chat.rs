//! Twitch chat commands for the streamer and moderators, e.g. `!delay on`.
//! Reads chat anonymously (no login needed) over IRC; only listens.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, bail};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::mpsc::UnboundedSender;

use crate::EngineMsg;
use crate::config::TwitchChat;
use crate::i18n::{self, Lang};
use crate::status::{Cmd, Shared};

/// Keeps a chat connection matching the current settings, reconnecting on changes.
pub async fn run(shared: Arc<Shared>, tx: UnboundedSender<EngineMsg>) {
    loop {
        let (on, cfg) = {
            let c = shared.config.lock().unwrap();
            (c.features.chat, c.twitch_chat.clone())
        };
        if !on || channel(&cfg).is_empty() {
            tokio::time::sleep(Duration::from_secs(3)).await;
            continue;
        }
        match listen(&shared, &tx, &cfg).await {
            Ok(()) => {} // settings changed
            Err(e) => {
                log::warn!("[chat] {e:#}");
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        }
    }
}

fn channel(cfg: &TwitchChat) -> String {
    channel_name(&cfg.channel)
}

/// Channel login from whatever was typed: "name", "@name", "#name",
/// "twitch.tv/name" or a full https://www.twitch.tv/name link.
pub fn channel_name(typed: &str) -> String {
    let mut c = typed.trim().to_lowercase();
    if let Some(i) = c.find("twitch.tv/") {
        c = c[i + "twitch.tv/".len()..].to_string();
    }
    c.trim_start_matches(['#', '@'])
        .split(['/', '?', '#', ' '])
        .next()
        .unwrap_or("")
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .collect()
}

async fn listen(shared: &Shared, tx: &UnboundedSender<EngineMsg>, cfg: &TwitchChat) -> Result<()> {
    let chan = channel(cfg);
    let stream = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect("irc.chat.twitch.tv:6667")).await??;
    let (r, mut w) = stream.into_split();
    let nick = format!("justinfan{}", 10_000 + std::process::id() % 80_000);
    w.write_all(format!("CAP REQ :twitch.tv/tags\r\nPASS SCHMOOPIIE\r\nNICK {nick}\r\nJOIN #{chan}\r\n").as_bytes()).await?;
    log::info!("[chat] listening to #{chan}");
    let mut lines = BufReader::new(r).lines();
    let mut check = tokio::time::interval(Duration::from_secs(3));
    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else { bail!("chat connection closed") };
                if line.starts_with("PING") {
                    w.write_all(line.replacen("PING", "PONG", 1).as_bytes()).await?;
                    w.write_all(b"\r\n").await?;
                    continue;
                }
                if line.contains(" 366 ") {
                    log::info!("[chat] joined #{chan}, waiting for commands");
                }
                if line.contains(" NOTICE ") {
                    log::warn!("[chat] {line}");
                }
                if let Some((user, cmd)) = parse_command(&line, cfg, i18n::get()) {
                    log::info!("[chat] {user}: {cmd:?}");
                    let _ = tx.send(EngineMsg::Cmd(cmd));
                    if let Cmd::Set(_) = cmd {
                        // "!delay 60" means "delay of 60 s": also turn it on
                        let _ = tx.send(EngineMsg::Cmd(Cmd::On));
                    }
                }
            }
            _ = check.tick() => {
                let c = shared.config.lock().unwrap();
                if !c.features.chat || c.twitch_chat != *cfg {
                    log::info!("[chat] settings changed, leaving #{chan}");
                    return Ok(());
                }
            }
        }
    }
}

/// Parses a tagged PRIVMSG; returns the sender and the command if they may use it.
/// The English words always work; the translated ones follow the app language
/// (`lang`), since "apagar" is "delete" in Portuguese but "turn off" in Spanish.
fn parse_command(line: &str, cfg: &TwitchChat, lang: Lang) -> Option<(String, Cmd)> {
    let (tags, rest) = line.strip_prefix('@')?.split_once(' ')?;
    let (prefix, rest) = rest.strip_prefix(':')?.split_once(' ')?;
    let rest = rest.strip_prefix("PRIVMSG ")?;
    let (_, text) = rest.split_once(" :")?;
    let user = prefix.split('!').next().unwrap_or("").to_string();

    let badges = tags.split(';').find_map(|t| t.strip_prefix("badges=")).unwrap_or("");
    let has = |b: &str| badges.split(',').any(|x| x.split('/').next() == Some(b));
    let allowed = match cfg.allow.as_str() {
        "broadcaster" => has("broadcaster"),
        "vips" => has("broadcaster") || has("moderator") || has("vip"),
        _ => has("broadcaster") || has("moderator"),
    };
    if !allowed {
        return None;
    }
    let prefix = cfg.prefix.trim().to_lowercase();
    let text = text.trim().to_lowercase();
    let args = text.strip_prefix(&prefix)?;
    if !(args.is_empty() || args.starts_with(char::is_whitespace)) {
        return None; // "!delayed" is not "!delay"
    }
    let mut it = args.split_whitespace();
    // a bare "!delay" does nothing: a mod checking the state must not flip it
    let word = it.next()?.to_lowercase();
    let arg = it.next().and_then(|n| n.trim_end_matches('s').parse::<u32>().ok());
    // second word, for "!delay 60 20m" / "!delay on 20m" / "!delay timer 20"
    let second = args.split_whitespace().nth(1);
    let cmd = match alias(&word, lang) {
        "on" => match second.and_then(minutes_with_unit) {
            Some(m) => Cmd::OnFor(m, None),
            None => Cmd::On,
        },
        "timer" | "autooff" => Cmd::AutoOff(second.and_then(|w| timer_minutes(w, lang))?),
        "off" => Cmd::Off,
        "toggle" => Cmd::Toggle,
        "censor" => Cmd::Censor(arg),
        "replay" => Cmd::Replay(arg),
        "clip" => Cmd::Clip(arg),
        "panic" => Cmd::Panic,
        n => match (n.trim_end_matches('s').parse::<u32>(), second.and_then(minutes_with_unit)) {
            (Ok(secs), Some(m)) => Cmd::OnFor(m, Some(secs)),
            (Ok(secs), None) => Cmd::Set(secs),
            (Err(_), _) => return None,
        },
    };
    Some((user, cmd))
}

/// The English command for a word of the chat language. English (and older
/// configs) keep the Portuguese words, which always worked.
fn alias(word: &str, lang: Lang) -> &str {
    let es = lang == Lang::Es;
    match word {
        "ligar" if !es => "on",
        "desligar" if !es => "off",
        "apagar" if !es => "censor",
        "clipe" if !es => "clip",
        "encender" | "activar" if es => "on",
        "apagar" | "desactivar" if es => "off",
        "alternar" if es => "toggle",
        "borrar" | "censurar" if es => "censor",
        "repetir" if es => "replay",
        "temporizador" if es => "timer",
        "panico" | "pânico" | "pánico" => "panic",
        w => w,
    }
}

/// "20m" / "20min" (the unit tells minutes apart from seconds).
fn minutes_with_unit(word: &str) -> Option<u32> {
    word.strip_suffix("min").or_else(|| word.strip_suffix('m'))?.parse().ok()
}

/// Minutes after "!delay timer": "20", "20m", or "off"/"0" to cancel.
fn timer_minutes(word: &str, lang: Lang) -> Option<u32> {
    match alias(word, lang) {
        "off" | "cancel" | "cancelar" => Some(0),
        w => minutes_with_unit(w).or_else(|| w.parse().ok()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(allow: &str) -> TwitchChat {
        TwitchChat { enabled: false, channel: "streamer".into(), allow: allow.into(), prefix: "!delay".into() }
    }

    fn parse_command_en(line: &str, cfg: &TwitchChat) -> Option<(String, Cmd)> {
        parse_command(line, cfg, Lang::En)
    }

    fn msg(badges: &str, text: &str) -> String {
        format!("@badge-info=;badges={badges};color=;display-name=Joe :joe!joe@joe.tmi.twitch.tv PRIVMSG #streamer :{text}")
    }

    #[test]
    fn mods_can_use_commands() {
        let c = cfg("mods");
        assert_eq!(parse_command_en(&msg("moderator/1", "!delay on"), &c).unwrap().1, Cmd::On);
        assert_eq!(parse_command_en(&msg("broadcaster/1", "!delay 45"), &c).unwrap().1, Cmd::Set(45));
        assert_eq!(parse_command_en(&msg("moderator/1", "!delay apagar 8"), &c).unwrap().1, Cmd::Censor(Some(8)));
        assert_eq!(parse_command_en(&msg("moderator/1", "!DELAY off"), &c).unwrap().1, Cmd::Off);
        assert_eq!(parse_command_en(&msg("moderator/1", "!delay toggle"), &c).unwrap().1, Cmd::Toggle);
        assert!(parse_command_en(&msg("moderator/1", "!delay"), &c).is_none());
        assert!(parse_command_en(&msg("moderator/1", "!delay   "), &c).is_none());
    }

    #[test]
    fn auto_off_commands() {
        let c = cfg("mods");
        let cmd = |text: &str| parse_command_en(&msg("moderator/1", text), &c).map(|x| x.1);
        assert_eq!(cmd("!delay 60 20m"), Some(Cmd::OnFor(20, Some(60))));
        assert_eq!(cmd("!delay 60s 90min"), Some(Cmd::OnFor(90, Some(60))));
        assert_eq!(cmd("!delay 60 20"), Some(Cmd::Set(60)), "no unit: not minutes");
        assert_eq!(cmd("!delay on 30m"), Some(Cmd::OnFor(30, None)));
        assert_eq!(cmd("!delay timer 20"), Some(Cmd::AutoOff(20)));
        assert_eq!(cmd("!delay timer 45m"), Some(Cmd::AutoOff(45)));
        assert_eq!(cmd("!delay timer off"), Some(Cmd::AutoOff(0)));
        assert_eq!(cmd("!delay timer"), None);
        assert_eq!(cmd("!delay timer soon"), None);
    }

    #[test]
    fn words_follow_the_language() {
        let c = cfg("mods");
        let cmd = |text: &str, lang| parse_command(&msg("moderator/1", text), &c, lang).map(|x| x.1);
        // English works in every language
        for lang in [Lang::En, Lang::Pt, Lang::Es] {
            assert_eq!(cmd("!delay on", lang), Some(Cmd::On));
            assert_eq!(cmd("!delay off", lang), Some(Cmd::Off));
            assert_eq!(cmd("!delay censor 5", lang), Some(Cmd::Censor(Some(5))));
            assert_eq!(cmd("!delay clip", lang), Some(Cmd::Clip(None)));
            assert_eq!(cmd("!delay timer off", lang), Some(Cmd::AutoOff(0)));
        }
        // Portuguese: "apagar" deletes the last seconds
        for lang in [Lang::Pt, Lang::En] {
            assert_eq!(cmd("!delay apagar 8", lang), Some(Cmd::Censor(Some(8))));
            assert_eq!(cmd("!delay ligar 20m", lang), Some(Cmd::OnFor(20, None)));
            assert_eq!(cmd("!delay desligar", lang), Some(Cmd::Off));
            assert_eq!(cmd("!delay clipe", lang), Some(Cmd::Clip(None)));
            assert_eq!(cmd("!delay pânico", lang), Some(Cmd::Panic));
            assert_eq!(cmd("!delay timer desligar", lang), Some(Cmd::AutoOff(0)));
            assert_eq!(cmd("!delay encender", lang), None);
        }
        // Spanish: "apagar" turns the delay off
        let es = Lang::Es;
        assert_eq!(cmd("!delay apagar", es), Some(Cmd::Off));
        assert_eq!(cmd("!delay apagar 8", es), Some(Cmd::Off));
        assert_eq!(cmd("!delay desactivar", es), Some(Cmd::Off));
        assert_eq!(cmd("!delay encender", es), Some(Cmd::On));
        assert_eq!(cmd("!delay activar 30m", es), Some(Cmd::OnFor(30, None)));
        assert_eq!(cmd("!delay alternar", es), Some(Cmd::Toggle));
        assert_eq!(cmd("!delay borrar 8", es), Some(Cmd::Censor(Some(8))));
        assert_eq!(cmd("!delay censurar", es), Some(Cmd::Censor(None)));
        assert_eq!(cmd("!delay repetir 15", es), Some(Cmd::Replay(Some(15))));
        assert_eq!(cmd("!delay pánico", es), Some(Cmd::Panic));
        assert_eq!(cmd("!delay temporizador 20", es), Some(Cmd::AutoOff(20)));
        assert_eq!(cmd("!delay timer apagar", es), Some(Cmd::AutoOff(0)));
        assert_eq!(cmd("!delay timer cancelar", es), Some(Cmd::AutoOff(0)));
        assert_eq!(cmd("!delay ligar", es), None);
        assert_eq!(cmd("!delay desligar", es), None);
    }

    #[test]
    fn channel_names_are_cleaned() {
        for typed in ["ragnar_cb", "@Ragnar_CB", "#ragnar_cb", "twitch.tv/ragnar_cb", "https://www.twitch.tv/ragnar_cb", "www.twitch.tv/ragnar_cb/videos", " ragnar_cb "] {
            assert_eq!(channel_name(typed), "ragnar_cb", "{typed}");
        }
    }

    #[test]
    fn broadcaster_badge_from_real_chat() {
        // the streamer typing in their own chat (broadcaster + subscriber badges)
        let line = "@badge-info=subscriber/71;badges=broadcaster/1,subscriber/3072,clips-leader/1;color=#1E90FF;display-name=ragnar_cb;mod=0 :ragnar_cb!ragnar_cb@ragnar_cb.tmi.twitch.tv PRIVMSG #ragnar_cb :!delay 10";
        assert_eq!(parse_command_en(line, &cfg("mods")).unwrap().1, Cmd::Set(10));
    }

    #[test]
    fn viewers_are_ignored() {
        let c = cfg("mods");
        assert!(parse_command_en(&msg("subscriber/12", "!delay off"), &c).is_none());
        assert!(parse_command_en(&msg("vip/1", "!delay off"), &c).is_none());
        assert!(parse_command_en(&msg("vip/1", "!delay off"), &cfg("vips")).is_some());
        assert!(parse_command_en(&msg("moderator/1", "!delay off"), &cfg("broadcaster")).is_none());
        assert!(parse_command_en(&msg("moderator/1", "hello"), &c).is_none());
        assert!(parse_command_en(&msg("moderator/1", "!delay banana"), &c).is_none());
    }
}
