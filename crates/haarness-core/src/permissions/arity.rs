//! Command-prefix arities: how many tokens identify the "human-readable
//! command" of a bash invocation. `git checkout main` → `git checkout`
//! (git has arity 2); `npm run dev x` → `npm run dev` (npm run has arity 3);
//! unknown commands fall back to their first token.
//!
//! "Allow always" uses these prefixes to learn stable command patterns.

use std::collections::HashMap;
use std::sync::OnceLock;

/// The prefix tokens that identify this command, per the arity table.
/// Longest matching prefix wins; flags never count as tokens upstream of
/// this call (the caller passes command tokens, not flags — see [`super::bash`]).
#[must_use]
pub fn prefix(tokens: &[String]) -> Vec<String> {
    let table = table();
    // No key is longer than the longest one, so a long command costs no more
    // than a short one.
    for length in (1..=tokens.len().min(longest_key())).rev() {
        let candidate = tokens[..length].join(" ");
        if let Some(&arity) = table.get(candidate.as_str()) {
            return tokens[..arity.min(tokens.len())].to_vec();
        }
    }
    tokens.first().cloned().into_iter().collect()
}

/// How many words the longest key in [`table`] has.
fn longest_key() -> usize {
    static LONGEST: OnceLock<usize> = OnceLock::new();
    *LONGEST.get_or_init(|| {
        table()
            .keys()
            .map(|key| key.split(' ').count())
            .max()
            .unwrap_or(1)
    })
}

#[allow(clippy::too_many_lines)]
fn table() -> &'static HashMap<&'static str, usize> {
    static TABLE: OnceLock<HashMap<&'static str, usize>> = OnceLock::new();
    TABLE.get_or_init(|| {
        HashMap::from([
            ("cat", 1),
            ("cd", 1),
            ("chmod", 1),
            ("chown", 1),
            ("cp", 1),
            ("echo", 1),
            ("env", 1),
            ("export", 1),
            ("grep", 1),
            ("kill", 1),
            ("killall", 1),
            ("ln", 1),
            ("ls", 1),
            ("mkdir", 1),
            ("mv", 1),
            ("ps", 1),
            ("pwd", 1),
            ("rm", 1),
            ("rmdir", 1),
            ("sleep", 1),
            ("source", 1),
            ("tail", 1),
            ("touch", 1),
            ("unset", 1),
            ("which", 1),
            ("aws", 3),
            ("az", 3),
            ("bazel", 2),
            ("brew", 2),
            ("bun", 2),
            ("bun run", 3),
            ("bun x", 3),
            ("cargo", 2),
            ("cargo add", 3),
            ("cargo run", 3),
            ("cdk", 2),
            ("cf", 2),
            ("cmake", 2),
            ("composer", 2),
            ("consul", 2),
            ("consul kv", 3),
            ("crictl", 2),
            ("deno", 2),
            ("deno task", 3),
            ("doctl", 3),
            ("docker", 2),
            ("docker builder", 3),
            ("docker compose", 3),
            ("docker container", 3),
            ("docker image", 3),
            ("docker network", 3),
            ("docker volume", 3),
            ("eksctl", 2),
            ("eksctl create", 3),
            ("firebase", 2),
            ("flyctl", 2),
            ("gcloud", 3),
            ("gh", 3),
            ("git", 2),
            ("git config", 3),
            ("git remote", 3),
            ("git stash", 3),
            ("go", 2),
            ("gradle", 2),
            ("helm", 2),
            ("heroku", 2),
            ("hugo", 2),
            ("ip", 2),
            ("ip addr", 3),
            ("ip link", 3),
            ("ip netns", 3),
            ("ip route", 3),
            ("kind", 2),
            ("kind create", 3),
            ("kubectl", 2),
            ("kubectl kustomize", 3),
            ("kubectl rollout", 3),
            ("kustomize", 2),
            ("make", 2),
            ("mc", 2),
            ("mc admin", 3),
            ("minikube", 2),
            ("mongosh", 2),
            ("mysql", 2),
            ("mvn", 2),
            ("ng", 2),
            ("npm", 2),
            ("npm exec", 3),
            ("npm init", 3),
            ("npm run", 3),
            ("npm view", 3),
            ("nvm", 2),
            ("nx", 2),
            ("openssl", 2),
            ("openssl req", 3),
            ("openssl x509", 3),
            ("pip", 2),
            ("pipenv", 2),
            ("pnpm", 2),
            ("pnpm dlx", 3),
            ("pnpm exec", 3),
            ("pnpm run", 3),
            ("poetry", 2),
            ("podman", 2),
            ("podman container", 3),
            ("podman image", 3),
            ("psql", 2),
            ("pulumi", 2),
            ("pulumi stack", 3),
            ("pyenv", 2),
            ("python", 2),
            ("rake", 2),
            ("rbenv", 2),
            ("redis-cli", 2),
            ("rustup", 2),
            ("serverless", 2),
            ("sfdx", 3),
            ("skaffold", 2),
            ("sls", 2),
            ("sst", 2),
            ("swift", 2),
            ("systemctl", 2),
            ("terraform", 2),
            ("terraform workspace", 3),
            ("tmux", 2),
            ("turbo", 2),
            ("ufw", 2),
            ("vault", 2),
            ("vault auth", 3),
            ("vault kv", 3),
            ("vercel", 2),
            ("volta", 2),
            ("wp", 2),
            ("yarn", 2),
            ("yarn dlx", 3),
            ("yarn run", 3),
        ])
    })
}

#[cfg(test)]
mod tests {
    use super::prefix;

    fn tokens(command: &str) -> Vec<String> {
        command.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn arity_prefixes_match_the_haarness_table() {
        assert_eq!(prefix(&tokens("git checkout main")), tokens("git checkout"));
        assert_eq!(prefix(&tokens("git status")), tokens("git status"));
        assert_eq!(prefix(&tokens("npm run dev extra")), tokens("npm run dev"));
        assert_eq!(prefix(&tokens("npm install")), tokens("npm install"));
        assert_eq!(prefix(&tokens("aws s3 ls bucket")), tokens("aws s3 ls"));
        assert_eq!(prefix(&tokens("touch foo.txt")), tokens("touch"));
        assert_eq!(prefix(&tokens("cargo test -p foo")), tokens("cargo test"));
        assert_eq!(
            prefix(&tokens("cargo add tokio")),
            tokens("cargo add tokio")
        );
        assert_eq!(
            prefix(&tokens("git config user.name")),
            tokens("git config user.name")
        );
        // Unknown commands: first token only.
        assert_eq!(
            prefix(&tokens("customtool build now")),
            tokens("customtool")
        );
        // Shorter than the arity: keep what exists.
        assert_eq!(prefix(&tokens("npm")), tokens("npm"));
        assert!(prefix(&[]).is_empty());
    }
}
