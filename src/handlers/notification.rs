//! Purpose: Allow any user to ping a pre-selected group of people on GitHub via comments.
//!
//! The set of "teams" which can be pinged is intentionally restricted via configuration.
//!
//! Parsing is done in the `parser::command::ping` module.

use crate::db::notifications;
use crate::{
    github::{self, Event},
    handlers::Context,
};
use anyhow::Context as _;
use regex::Regex;
use std::collections::HashSet;
use std::convert::TryFrom;

lazy_static::lazy_static! {
    static ref PING_RE: Regex = Regex::new(r#"@([-\w\d/]+)"#,).unwrap();
    static ref ACKNOWLEDGE_RE: Regex = Regex::new(r#"acknowledge (https?://[^ ]+)"#,).unwrap();
}

pub async fn handle(ctx: &Context, event: &Event) -> anyhow::Result<()> {
    let body = match event.comment_body() {
        Some(v) => v,
        // Skip events that don't have comment bodies associated
        None => return Ok(()),
    };

    // Permit editing acknowledgement

    let acks = ACKNOWLEDGE_RE
        .captures_iter(body)
        .map(|c| c.get(1).unwrap().as_str().to_owned())
        .collect::<Vec<_>>();
    log::trace!("Captured acknowledgements: {:?}", acks);
    for url in acks {
        let user = match event {
            Event::Issue(e) => &e.issue.user,
            Event::IssueComment(e) => &e.comment.user,
        };
        let id = match user.id {
            Some(id) => id,
            // If the user was not in the team(s) then just don't record it.
            None => {
                log::trace!("Skipping {} because no id found", user.login);
                return Ok(());
            }
        };

        if let Err(e) = notifications::delete_ping(
            &mut Context::make_db_client(&ctx.github.raw()).await?,
            id,
            notifications::Identifier::Url(&url),
        )
        .await
        {
            log::warn!(
                "failed to delete notification: url={}, user={:?}: {:?}",
                url,
                user,
                e
            );
        }
    }

    if let Event::Issue(e) = event {
        if e.action != github::IssuesAction::Opened {
            // skip events other than opening the issue to avoid retriggering commands in the
            // issue body
            return Ok(());
        }
    }

    if let Event::IssueComment(e) = event {
        if e.action != github::IssueCommentAction::Created {
            // skip events other than creating a comment to avoid
            // renotifying
            //
            // FIXME: implement smart tracking to allow rerunning only if
            // the notification is "new" (i.e. edit adds a ping)
            return Ok(());
        }
    }

    let short_description = match event {
        Event::Issue(e) => e.issue.title.clone(),
        Event::IssueComment(e) => format!("Comment on {}", e.issue.title),
    };

    let caps = PING_RE
        .captures_iter(body)
        .map(|c| c.get(1).unwrap().as_str().to_owned())
        .collect::<HashSet<_>>();
    let mut users_notified = HashSet::new();
    log::trace!("Captured usernames in comment: {:?}", caps);
    for login in caps {
        let (users, team_name) = if login.contains('/') {
            // This is a team ping. For now, just add it to everyone's agenda on
            // that team, but also mark it as such (i.e., a team ping) for
            // potentially different prioritization and so forth.
            //
            // In order to properly handle this down the road, we will want to
            // distinguish between "everyone must pay attention" and "someone
            // needs to take a look."
            //
            // We may also want to be able to categorize into these buckets
            // *after* the ping occurs and is initially processed.

            let mut iter = login.split('/');
            let _rust_lang = iter.next().unwrap();
            let team = iter.next().unwrap();
            let team = match github::get_team(&ctx.github, team).await {
                Ok(Some(team)) => team,
                Ok(None) => {
                    log::error!("team ping ({}) failed to resolve to a known team", login);
                    continue;
                }
                Err(err) => {
                    log::error!(
                        "team ping ({}) failed to resolve to a known team: {:?}",
                        login,
                        err
                    );
                    continue;
                }
            };

            (
                team.members
                    .into_iter()
                    .map(|member| {
                        let id = i64::try_from(member.github_id).with_context(|| {
                            format!("user id {} out of bounds", member.github_id)
                        })?;
                        Ok(github::User {
                            id: Some(id),
                            login: member.github,
                        })
                    })
                    .collect::<anyhow::Result<Vec<github::User>>>(),
                Some(team.name),
            )
        } else {
            let user = github::User { login, id: None };
            let id = user
                .get_id(&ctx.github)
                .await
                .with_context(|| format!("failed to get user {} ID", user.login))?;
            let id = match id {
                Some(id) => id,
                // If the user was not in the team(s) then just don't record it.
                None => {
                    log::trace!("Skipping {} because no id found", user.login);
                    continue;
                }
            };
            let id = i64::try_from(id).with_context(|| format!("user id {} out of bounds", id));
            (
                id.map(|id| {
                    vec![github::User {
                        login: user.login.clone(),
                        id: Some(id),
                    }]
                }),
                None,
            )
        };

        let users = match users {
            Ok(users) => users,
            Err(err) => {
                log::error!("getting users failed: {:?}", err);
                continue;
            }
        };

        for user in users {
            if !users_notified.insert(user.id.unwrap()) {
                // Skip users already associated with this event.
                continue;
            }

            if let Err(err) = notifications::record_username(&ctx.db, user.id.unwrap(), user.login)
                .await
                .context("failed to record username")
            {
                log::error!("record username: {:?}", err);
            }

            if let Err(err) = notifications::record_ping(
                &ctx.db,
                &notifications::Notification {
                    user_id: user.id.unwrap(),
                    origin_url: event.html_url().unwrap().to_owned(),
                    origin_html: body.to_owned(),
                    time: event.time(),
                    short_description: Some(short_description.clone()),
                    team_name: team_name.clone(),
                },
            )
            .await
            .context("failed to record ping")
            {
                log::error!("record ping: {:?}", err);
            }
        }
    }

    Ok(())
}