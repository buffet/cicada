use errno::errno;
use libc;
use std::collections::HashMap;
use std::env;
use std::io::Write;
use std::mem;

use glob;
use regex::Regex;

use crate::execute;
use crate::libs;
use crate::parsers;
use crate::tools::{self, clog};
use crate::types;

#[derive(Debug, Clone)]
pub struct Shell {
    pub jobs: HashMap<i32, types::Job>,
    pub alias: HashMap<String, String>,
    pub envs: HashMap<String, String>,
    pub cmd: String,
    pub previous_dir: String,
    pub previous_cmd: String,
    pub previous_status: i32,
}

impl Shell {
    pub fn new() -> Shell {
        Shell {
            jobs: HashMap::new(),
            alias: HashMap::new(),
            envs: HashMap::new(),
            cmd: String::new(),
            previous_dir: String::new(),
            previous_cmd: String::new(),
            previous_status: 0,
        }
    }

    pub fn insert_job(&mut self, gid: i32, pid: i32, cmd: &str, status: &str, bg: bool) {
        let mut i = 1;
        loop {
            let mut indexed_job_missing = false;
            if let Some(x) = self.jobs.get_mut(&i) {
                if x.gid == gid {
                    x.pids.push(pid);
                    return;
                }
            } else {
                indexed_job_missing = true;
            }

            let mut _cmd = cmd.to_string();
            if bg && !_cmd.ends_with('&') {
                _cmd.push_str(" &");
            }
            if indexed_job_missing {
                self.jobs.insert(
                    i,
                    types::Job {
                        cmd: _cmd.to_string(),
                        id: i,
                        gid: gid,
                        pids: vec![pid],
                        status: status.to_string(),
                        report: bg,
                    },
                );
                return;
            }
            i += 1;
        }
    }

    pub fn get_job_by_id(&self, job_id: i32) -> Option<&types::Job> {
        self.jobs.get(&job_id)
    }

    pub fn get_job_by_gid(&self, gid: i32) -> Option<&types::Job> {
        if self.jobs.is_empty() {
            return None;
        }

        let mut i = 1;
        loop {
            if let Some(x) = self.jobs.get(&i) {
                if x.gid == gid {
                    return Some(&x);
                }
            }

            i += 1;
            if i >= 65535 {
                break;
            }
        }
        None
    }

    pub fn mark_job_as_running(&mut self, gid: i32, bg: bool) {
        if self.jobs.is_empty() {
            return;
        }

        let mut i = 1;
        loop {
            if let Some(x) = self.jobs.get_mut(&i) {
                if x.gid == gid {
                    x.status = "Running".to_string();
                    x.report = bg;
                    if bg && !x.cmd.ends_with(" &") {
                        x.cmd = format!("{} &", x.cmd);
                    }
                    return;
                }
            }

            i += 1;
            if i >= 65535 {
                break;
            }
        }
    }

    pub fn mark_job_as_stopped(&mut self, gid: i32) {
        if self.jobs.is_empty() {
            return;
        }

        let mut i = 1;
        loop {
            if let Some(x) = self.jobs.get_mut(&i) {
                if x.gid == gid {
                    x.status = "Stopped".to_string();
                    return;
                }
            }

            i += 1;
            if i >= 65535 {
                break;
            }
        }
    }

    pub fn remove_pid_from_job(&mut self, gid: i32, pid: i32) -> Option<types::Job> {
        if self.jobs.is_empty() {
            return None;
        }

        let mut empty_pids = false;
        let mut i = 1;
        loop {
            if let Some(x) = self.jobs.get_mut(&i) {
                if x.gid == gid {
                    if let Ok(i_pid) = x.pids.binary_search(&pid) {
                        x.pids.remove(i_pid);
                    }
                    empty_pids = x.pids.is_empty();
                    break;
                }
            }

            i += 1;
            if i >= 65535 {
                break;
            }
        }

        if empty_pids {
            return self.jobs.remove(&i);
        }
        None
    }

    pub fn set_env(&mut self, name: &str, value: &str) {
        if env::var(name).is_ok() {
            env::set_var(name, value);
        } else {
            self.envs.insert(name.to_string(), value.to_string());
        }
    }

    pub fn get_env(&self, name: &str) -> Option<String> {
        match self.envs.get(name) {
            Some(x) => Some(x.to_string()),
            None => None,
        }
    }

    pub fn add_alias(&mut self, name: &str, value: &str) {
        self.alias.insert(name.to_string(), value.to_string());
    }

    pub fn is_alias(&self, name: &str) -> bool {
        self.alias.contains_key(name)
    }

    pub fn get_alias_content(&self, name: &str) -> Option<String> {
        let result;
        match self.alias.get(name) {
            Some(x) => {
                result = x.to_string();
            }
            None => {
                result = String::new();
            }
        }
        if result.is_empty() {
            None
        } else {
            Some(result)
        }
    }
}

pub unsafe fn give_terminal_to(gid: i32) -> bool {
    let mut mask: libc::sigset_t = mem::zeroed();
    let mut old_mask: libc::sigset_t = mem::zeroed();

    libc::sigemptyset(&mut mask);
    libc::sigaddset(&mut mask, libc::SIGTSTP);
    libc::sigaddset(&mut mask, libc::SIGTTIN);
    libc::sigaddset(&mut mask, libc::SIGTTOU);
    libc::sigaddset(&mut mask, libc::SIGCHLD);

    let rcode = libc::pthread_sigmask(libc::SIG_BLOCK, &mask, &mut old_mask);
    if rcode != 0 {
        log!("failed to call pthread_sigmask");
    }
    let rcode = libc::tcsetpgrp(1, gid);
    let given;
    if rcode == -1 {
        given = false;
        let e = errno();
        let code = e.0;
        log!("error in give_terminal_to() {}: {}", code, e);
    } else {
        given = true;
    }
    let rcode = libc::pthread_sigmask(libc::SIG_SETMASK, &old_mask, &mut mask);
    if rcode != 0 {
        log!("failed to call pthread_sigmask");
    }
    given
}

fn needs_globbing(line: &str) -> bool {
    if tools::is_arithmetic(line) {
        return false;
    }

    let re;
    if let Ok(x) = Regex::new(r"\*+") {
        re = x;
    } else {
        return false;
    }

    let tokens = parsers::parser_line::cmd_to_tokens(line);
    for (sep, token) in tokens {
        if !sep.is_empty() {
            continue;
        }
        if re.is_match(&token) {
            return true;
        }
    }
    false
}

pub fn expand_glob(tokens: &mut types::Tokens) {
    let mut idx: usize = 0;

    let mut buff: HashMap<usize, Vec<String>> = HashMap::new();
    for (sep, text) in tokens.iter() {
        if !sep.is_empty() || !needs_globbing(text) {
            idx += 1;
            continue;
        }

        let _line = text.to_string();
        // XXX: spliting needs to consider cases like `echo 'a * b'`
        let _tokens: Vec<&str> = _line.split(' ').collect();
        let mut result: Vec<String> = Vec::new();
        for item in &_tokens {
            if !item.contains('*') || item.trim().starts_with('\'') || item.trim().starts_with('"')
            {
                result.push(item.to_string());
            } else {
                let _basename = libs::path::basename(item);
                let show_hidden = _basename.starts_with(".*");

                match glob::glob(item) {
                    Ok(paths) => {
                        let mut is_empty = true;
                        for entry in paths {
                            match entry {
                                Ok(path) => {
                                    let file_path = path.to_string_lossy();
                                    let _basename = libs::path::basename(&file_path);
                                    if _basename == ".." || _basename == "." {
                                        continue;
                                    }
                                    if _basename.starts_with('.') && !show_hidden {
                                        // skip hidden files, you may need to
                                        // type `ls .*rc` instead of `ls *rc`
                                        continue;
                                    }
                                    result.push(file_path.to_string());
                                    is_empty = false;
                                }
                                Err(e) => {
                                    log!("glob error: {:?}", e);
                                }
                            }
                        }
                        if is_empty {
                            result.push(item.to_string());
                        }
                    }
                    Err(e) => {
                        println!("glob error: {:?}", e);
                        result.push(item.to_string());
                        return;
                    }
                }
            }
        }

        buff.insert(idx, result);
        idx += 1;
    }

    for (i, result) in buff.iter() {
        tokens.remove(*i as usize);
        for (j, token) in result.iter().enumerate() {
            let sep = if token.contains(' ') { "\"" } else { "" };
            tokens.insert((*i + j) as usize, (sep.to_string(), token.clone()));
        }
    }
}

pub fn extend_env_blindly(sh: &Shell, token: &str) -> String {
    let re;
    if let Ok(x) = Regex::new(r"([^\$]*)\$\{?([A-Za-z0-9\?\$_]+)\}?(.*)") {
        re = x;
    } else {
        println!("cicada: re new error");
        return String::new();
    }
    if !re.is_match(token) {
        return token.to_string();
    }

    let mut result = String::new();
    let mut _token = token.to_string();
    let mut _head = String::new();
    let mut _output = String::new();
    let mut _tail = String::new();
    loop {
        if !re.is_match(&_token) {
            if !_token.is_empty() {
                result.push_str(&_token);
            }
            break;
        }
        for cap in re.captures_iter(&_token) {
            _head = cap[1].to_string();
            _tail = cap[3].to_string();
            let _key = cap[2].to_string();
            if _key == "?" {
                result.push_str(format!("{}{}", _head, sh.previous_status).as_str());
            } else if _key == "$" {
                unsafe {
                    let val = libc::getpid();
                    result.push_str(format!("{}{}", _head, val).as_str());
                }
            } else if let Ok(val) = env::var(&_key) {
                result.push_str(format!("{}{}", _head, val).as_str());
            } else if let Some(val) = sh.get_env(&_key) {
                result.push_str(format!("{}{}", _head, val).as_str());
            } else {
                result.push_str(&_head);
            }
        }

        if _tail.is_empty() {
            break;
        }
        _token = _tail.clone();
    }
    result
}

fn expand_brace(tokens: &mut types::Tokens) {
    let mut idx: usize = 0;
    let mut buff: HashMap<usize, Vec<String>> = HashMap::new();
    for (sep, line) in tokens.iter() {
        if !sep.is_empty() || !tools::should_extend_brace(&line) {
            idx += 1;
            continue;
        }

        let _line = line.clone();
        let args = parsers::parser_line::cmd_to_tokens(_line.as_str());
        let mut result: Vec<String> = Vec::new();
        for (sep, token) in args {
            if sep.is_empty() && tools::should_extend_brace(token.as_str()) {
                let mut _prefix = String::new();
                let mut _token = String::new();
                let mut _result = Vec::new();
                let mut only_tail_left = false;
                let mut start_sign_found = false;
                for c in token.chars() {
                    if c == '{' {
                        start_sign_found = true;
                        continue;
                    }
                    if !start_sign_found {
                        _prefix.push(c);
                        continue;
                    }
                    if only_tail_left {
                        _token.push(c);
                        continue;
                    }
                    if c == '}' {
                        if !_token.is_empty() {
                            _result.push(_token);
                            _token = String::new();
                        }
                        only_tail_left = true;
                        continue;
                    }
                    if c == ',' {
                        if !_token.is_empty() {
                            _result.push(_token);
                            _token = String::new();
                        }
                    } else {
                        _token.push(c);
                    }
                }
                for item in &mut _result {
                    *item = format!("{}{}{}", _prefix, item, _token);
                }
                for item in _result.iter() {
                    result.push(item.clone());
                }
            } else {
                result.push(tools::wrap_sep_string(&sep, &token));
            }
        }

        buff.insert(idx, result);
        idx += 1;
    }

    for (i, result) in buff.iter() {
        tokens.remove(*i as usize);
        for (j, token) in result.iter().enumerate() {
            let sep = if token.contains(' ') { "\"" } else { "" };
            tokens.insert((*i + j) as usize, (sep.to_string(), token.clone()));
        }
    }
}

pub fn expand_home_string(text: &mut String) {
    let v = vec![
        r"(?P<head> +)~(?P<tail> +)",
        r"(?P<head> +)~(?P<tail>/)",
        r"^(?P<head> *)~(?P<tail>/)",
        r"(?P<head> +)~(?P<tail> *$)",
    ];
    for item in &v {
        let re;
        if let Ok(x) = Regex::new(item) {
            re = x;
        } else {
            return;
        }
        let home = tools::get_user_home();
        let ss = text.clone();
        let to = format!("$head{}$tail", home);
        let result = re.replace_all(ss.as_str(), to.as_str());
        *text = result.to_string();
    }
}

fn expand_alias(sh: &Shell, tokens: &mut types::Tokens) {
    let mut idx: usize = 0;
    let mut buff = Vec::new();
    let mut is_head = true;
    for (sep, text) in tokens.iter() {
        if sep.is_empty() && text == "|" {
            is_head = true;
            idx += 1;
            continue;
        }

        if !is_head || !sh.is_alias(&text) {
            idx += 1;
            is_head = false;
            continue;
        }

        if let Some(value) = sh.get_alias_content(&text) {
            buff.push((idx, value.clone()));
        }

        idx += 1;
        is_head = false;
    }

    for (i, text) in buff.iter().rev() {
        let tokens_ = parsers::parser_line::cmd_to_tokens(&text);
        tokens.remove(*i as usize);
        for item in tokens_.iter().rev() {
            tokens.insert(*i as usize, item.clone());
        }
    }
}

fn expand_home(tokens: &mut types::Tokens) {
    let mut idx: usize = 0;

    let mut buff: HashMap<usize, String> = HashMap::new();
    for (sep, text) in tokens.iter() {
        if !sep.is_empty() || !needs_expand_home(&text) {
            idx += 1;
            continue;
        }

        let mut s: String = text.clone();
        let v = vec![
            r"(?P<head> +)~(?P<tail> +)",
            r"(?P<head> +)~(?P<tail>/)",
            r"^(?P<head> *)~(?P<tail>/)",
            r"(?P<head> +)~(?P<tail> *$)",
        ];
        for item in &v {
            let re;
            if let Ok(x) = Regex::new(item) {
                re = x;
            } else {
                return;
            }
            let home = tools::get_user_home();
            let ss = s.clone();
            let to = format!("$head{}$tail", home);
            let result = re.replace_all(ss.as_str(), to.as_str());
            s = result.to_string();
        }
        buff.insert(idx, s.clone());
        idx += 1;
    }

    for (i, text) in buff.iter() {
        tokens[*i as usize].1 = text.to_string();
    }
}

fn env_in_token(token: &str) -> bool {
    if token == "$$" || token == "$?" {
        return true;
    }
    tools::re_contains(token, r"\$\{?[a-zA-Z][a-zA-Z0-9_]+\}?")
}

pub fn expand_env(sh: &Shell, tokens: &mut types::Tokens) {
    let mut idx: usize = 0;
    let mut buff: HashMap<usize, String> = HashMap::new();

    for (sep, token) in tokens.iter() {
        if sep == "`" || sep == "'" || !env_in_token(token) {
            idx += 1;
            continue;
        }

        let _token = extend_env_blindly(sh, token);
        buff.insert(idx, _token);
        idx += 1;
    }

    for (i, text) in buff.iter() {
        tokens[*i as usize].1 = text.to_string();
    }
}

fn should_do_dollar_command_extension(line: &str) -> bool {
    tools::re_contains(line, r"\$\([^\)]+\)")
}

fn do_command_substitution_for_dollar(sh: &mut Shell, tokens: &mut types::Tokens) {
    let mut idx: usize = 0;
    let mut buff: HashMap<usize, String> = HashMap::new();

    for (sep, token) in tokens.iter() {
        if sep == "'" || sep == "\\" || !should_do_dollar_command_extension(token) {
            idx += 1;
            continue;
        }

        let mut line = token.to_string();
        loop {
            if !should_do_dollar_command_extension(&line) {
                break;
            }
            let ptn_cmd = r"\$\(([^\(]+)\)";
            let cmd;
            match libs::re::find_first_group(ptn_cmd, &line) {
                Some(x) => {
                    cmd = x;
                }
                None => {
                    println_stderr!("cicada: no first group");
                    return;
                }
            }

            log!("run subcmd: {:?}", &cmd);
            let _args = parsers::parser_line::cmd_to_tokens(&cmd);
            let (_, cmd_result) =
                execute::run_pipeline(sh, &_args, "", false, false, true, false, None);
            let output_txt = cmd_result.stdout.trim();

            let ptn = r"(?P<head>[^\$]*)\$\([^\(]+\)(?P<tail>.*)";
            let re;
            if let Ok(x) = Regex::new(ptn) {
                re = x;
            } else {
                return;
            }

            let to = format!("${{head}}{}${{tail}}", output_txt);
            let line_ = line.clone();
            let result = re.replace(&line_, to.as_str());
            line = result.to_string();
        }

        buff.insert(idx, line.clone());
        idx += 1;
    }

    for (i, text) in buff.iter() {
        tokens[*i as usize].1 = text.to_string();
    }
}

fn do_command_substitution_for_dot(sh: &mut Shell, tokens: &mut types::Tokens) {
    let mut idx: usize = 0;
    let mut buff: HashMap<usize, String> = HashMap::new();
    for (sep, token) in tokens.iter() {
        let new_token: String;
        if sep == "`" {
            log!("run subcmd: {:?}", token);
            let _args = parsers::parser_line::cmd_to_tokens(&token);
            let (_, cr) = execute::run_pipeline(sh, &_args, "", false, false, true, false, None);
            new_token = cr.stdout.trim().to_string();
        } else if sep == "\"" || sep.is_empty() {
            let re;
            if let Ok(x) = Regex::new(r"^([^`]*)`([^`]+)`(.*)$") {
                re = x;
            } else {
                println_stderr!("cicada: re new error");
                return;
            }
            if !re.is_match(&token) {
                idx += 1;
                continue;
            }
            let mut _token = token.clone();
            let mut _item = String::new();
            let mut _head = String::new();
            let mut _output = String::new();
            let mut _tail = String::new();
            loop {
                if !re.is_match(&_token) {
                    if !_token.is_empty() {
                        _item = format!("{}{}", _item, _token);
                    }
                    break;
                }
                for cap in re.captures_iter(&_token) {
                    _head = cap[1].to_string();
                    _tail = cap[3].to_string();
                    log!("run subcmd: {:?}", &cap[2]);
                    let _args = parsers::parser_line::cmd_to_tokens(&cap[2]);
                    let (_, cr) =
                        execute::run_pipeline(sh, &_args, "", false, false, true, false, None);
                    _output = cr.stdout.trim().to_string();
                }
                _item = format!("{}{}{}", _item, _head, _output);
                if _tail.is_empty() {
                    break;
                }
                _token = _tail.clone();
            }
            new_token = _item;
        } else {
            idx += 1;
            continue;
        }

        buff.insert(idx, new_token.clone());
        idx += 1;
    }

    for (i, text) in buff.iter() {
        tokens[*i as usize].1 = text.to_string();
    }
}

fn do_command_substitution(sh: &mut Shell, tokens: &mut types::Tokens) {
    do_command_substitution_for_dot(sh, tokens);
    do_command_substitution_for_dollar(sh, tokens);
}

pub fn do_expansion(sh: &mut Shell, tokens: &mut types::Tokens) {
    if tokens.len() >= 2 {
        if tokens[0].1 == "export" && tokens[1].1.starts_with("PROMPT=") {
            return;
        }
    }

    expand_alias(sh, tokens);
    expand_home(tokens);
    expand_brace(tokens);
    expand_env(sh, tokens);
    expand_glob(tokens);
    do_command_substitution(sh, tokens);
}

pub fn needs_expand_home(line: &str) -> bool {
    tools::re_contains(line, r"( +~ +)|( +~/)|(^ *~/)|( +~ *$)")
}

#[cfg(test)]
mod tests {
    use super::expand_alias;
    use super::needs_expand_home;
    use super::needs_globbing;
    use super::should_do_dollar_command_extension;
    use super::Shell;

    #[test]
    fn test_need_expand_home() {
        assert!(needs_expand_home("ls ~"));
        assert!(needs_expand_home("ls  ~  "));
        assert!(needs_expand_home("cat ~/a.py"));
        assert!(needs_expand_home("echo ~"));
        assert!(needs_expand_home("echo ~ ~~"));
        assert!(needs_expand_home("~/bin/py"));
        assert!(!needs_expand_home("echo '~'"));
        assert!(!needs_expand_home("echo \"~\""));
        assert!(!needs_expand_home("echo ~~"));
    }

    #[test]
    fn test_needs_globbing() {
        assert!(needs_globbing("*"));
        assert!(needs_globbing("ls *"));
        assert!(needs_globbing("ls  *.txt"));
        assert!(needs_globbing("grep -i 'desc' /etc/*release*"));
        assert!(!needs_globbing("2 * 3"));
        assert!(!needs_globbing("ls '*.md'"));
        assert!(!needs_globbing("ls 'a * b'"));
        assert!(!needs_globbing("ls foo"));
    }

    #[test]
    fn test_should_do_dollar_command_extension() {
        assert!(!should_do_dollar_command_extension("ls $HOME"));
        assert!(!should_do_dollar_command_extension("echo $[pwd]"));
        assert!(should_do_dollar_command_extension("echo $(pwd)"));
        assert!(should_do_dollar_command_extension("echo $(pwd) foo"));
        assert!(should_do_dollar_command_extension("echo $(foo bar)"));
        assert!(should_do_dollar_command_extension("echo $(echo foo)"));
        assert!(should_do_dollar_command_extension("$(pwd) foo"));
    }

    #[test]
    fn test_expand_alias() {
        let mut sh = Shell::new();
        sh.add_alias("ls", "ls --color=auto");
        sh.add_alias("wc", "wc -l");

        let mut tokens = vec![
            ("".to_string(), "ls".to_string()),
            ("".to_string(), "|".to_string()),
            ("".to_string(), "wc".to_string()),
        ];
        let exp_tokens = vec![
            ("".to_string(), "ls".to_string()),
            ("".to_string(), "--color=auto".to_string()),
            ("".to_string(), "|".to_string()),
            ("".to_string(), "wc".to_string()),
            ("".to_string(), "-l".to_string()),
        ];
        expand_alias(&sh, &mut tokens);
        assert_eq!(tokens, exp_tokens);

        let mut tokens = vec![
            ("".to_string(), "which".to_string()),
            ("".to_string(), "ls".to_string()),
        ];
        let exp_tokens = vec![
            ("".to_string(), "which".to_string()),
            ("".to_string(), "ls".to_string()),
        ];
        expand_alias(&sh, &mut tokens);
        assert_eq!(tokens, exp_tokens);
    }
}
