use askama::Template;

use super::service::EmailMessage;

// --- Password Reset ---

#[derive(Template)]
#[template(path = "email/password_reset.html")]
struct PasswordResetHtml<'a> {
    reset_link: &'a str,
    app_name: &'a str,
}

#[derive(Template)]
#[template(path = "email/password_reset.txt")]
struct PasswordResetText<'a> {
    reset_link: &'a str,
    app_name: &'a str,
}

/// Build a password-reset email with both HTML and plaintext bodies.
pub fn password_reset_email(to: &str, reset_link: &str, app_name: &str) -> EmailMessage {
    let html_template = PasswordResetHtml {
        reset_link,
        app_name,
    };
    let text_template = PasswordResetText {
        reset_link,
        app_name,
    };

    EmailMessage {
        to: to.to_string(),
        subject: format!("{} — Reset Your Password", app_name),
        html_body: html_template.render().expect("failed to render password_reset HTML template"),
        text_body: text_template.render().expect("failed to render password_reset text template"),
    }
}

// --- Email Change Verify ---

#[derive(Template)]
#[template(path = "email/email_change_verify.html")]
struct EmailChangeVerifyHtml<'a> {
    verify_link: &'a str,
    app_name: &'a str,
}

#[derive(Template)]
#[template(path = "email/email_change_verify.txt")]
struct EmailChangeVerifyText<'a> {
    verify_link: &'a str,
    app_name: &'a str,
}

/// Build an email-change verification email with both HTML and plaintext bodies.
pub fn email_change_verify_email(to: &str, verify_link: &str, app_name: &str) -> EmailMessage {
    let html_template = EmailChangeVerifyHtml {
        verify_link,
        app_name,
    };
    let text_template = EmailChangeVerifyText {
        verify_link,
        app_name,
    };

    EmailMessage {
        to: to.to_string(),
        subject: format!("{} — Verify Your New Email Address", app_name),
        html_body: html_template.render().expect("failed to render email_change_verify HTML template"),
        text_body: text_template.render().expect("failed to render email_change_verify text template"),
    }
}

// --- Email Change Notification ---

#[derive(Template)]
#[template(path = "email/email_change_notification.html")]
struct EmailChangeNotificationHtml<'a> {
    new_email: &'a str,
    app_name: &'a str,
}

#[derive(Template)]
#[template(path = "email/email_change_notification.txt")]
struct EmailChangeNotificationText<'a> {
    new_email: &'a str,
    app_name: &'a str,
}

/// Build an email-change notification email with both HTML and plaintext bodies.
pub fn email_change_notification_email(to: &str, new_email: &str, app_name: &str) -> EmailMessage {
    let html_template = EmailChangeNotificationHtml {
        new_email,
        app_name,
    };
    let text_template = EmailChangeNotificationText {
        new_email,
        app_name,
    };

    EmailMessage {
        to: to.to_string(),
        subject: format!("{} — Your Email Address Was Changed", app_name),
        html_body: html_template.render().expect("failed to render email_change_notification HTML template"),
        text_body: text_template.render().expect("failed to render email_change_notification text template"),
    }
}

// --- Admin Invite ---

#[derive(Template)]
#[template(path = "email/admin_invite.html")]
struct AdminInviteHtml<'a> {
    invite_link: &'a str,
    app_name: &'a str,
}

#[derive(Template)]
#[template(path = "email/admin_invite.txt")]
struct AdminInviteText<'a> {
    invite_link: &'a str,
    app_name: &'a str,
}

/// Build an admin-invite email with both HTML and plaintext bodies.
pub fn admin_invite_email(to: &str, invite_link: &str, app_name: &str) -> EmailMessage {
    let html_template = AdminInviteHtml {
        invite_link,
        app_name,
    };
    let text_template = AdminInviteText {
        invite_link,
        app_name,
    };

    EmailMessage {
        to: to.to_string(),
        subject: format!("{} — Admin Invitation", app_name),
        html_body: html_template.render().expect("failed to render admin_invite HTML template"),
        text_body: text_template.render().expect("failed to render admin_invite text template"),
    }
}

// --- Team Invite ---

#[derive(Template)]
#[template(path = "email/team_invite.html")]
struct TeamInviteHtml<'a> {
    invite_link: &'a str,
    workspace_name: &'a str,
    inviter_name: &'a str,
    app_name: &'a str,
}

#[derive(Template)]
#[template(path = "email/team_invite.txt")]
struct TeamInviteText<'a> {
    invite_link: &'a str,
    workspace_name: &'a str,
    inviter_name: &'a str,
    app_name: &'a str,
}

/// Build a team-invite email with both HTML and plaintext bodies.
pub fn team_invite_email(
    to: &str,
    invite_link: &str,
    workspace_name: &str,
    inviter_name: &str,
    app_name: &str,
) -> EmailMessage {
    let html_template = TeamInviteHtml {
        invite_link,
        workspace_name,
        inviter_name,
        app_name,
    };
    let text_template = TeamInviteText {
        invite_link,
        workspace_name,
        inviter_name,
        app_name,
    };

    EmailMessage {
        to: to.to_string(),
        subject: format!("{} — You've Been Invited to {}", app_name, workspace_name),
        html_body: html_template.render().expect("failed to render team_invite HTML template"),
        text_body: text_template.render().expect("failed to render team_invite text template"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_password_reset_email_renders() {
        let msg = password_reset_email("user@example.com", "https://app.example.com/reset?token=abc123", "Ursnip");
        assert_eq!(msg.to, "user@example.com");
        assert_eq!(msg.subject, "Ursnip — Reset Your Password");
        assert!(msg.html_body.contains("https://app.example.com/reset?token=abc123"));
        assert!(msg.text_body.contains("https://app.example.com/reset?token=abc123"));
        assert!(msg.html_body.contains("30 minutes"));
        assert!(msg.text_body.contains("30 minutes"));
    }

    #[test]
    fn test_email_change_verify_email_renders() {
        let msg = email_change_verify_email("new@example.com", "https://app.example.com/verify?token=xyz", "Ursnip");
        assert_eq!(msg.to, "new@example.com");
        assert!(msg.html_body.contains("https://app.example.com/verify?token=xyz"));
        assert!(msg.text_body.contains("24 hours"));
    }

    #[test]
    fn test_email_change_notification_email_renders() {
        let msg = email_change_notification_email("old@example.com", "new@example.com", "Ursnip");
        assert_eq!(msg.to, "old@example.com");
        assert!(msg.html_body.contains("new@example.com"));
        assert!(msg.text_body.contains("new@example.com"));
    }

    #[test]
    fn test_admin_invite_email_renders() {
        let msg = admin_invite_email("admin@example.com", "https://app.example.com/invite?token=inv1", "Ursnip");
        assert_eq!(msg.to, "admin@example.com");
        assert!(msg.html_body.contains("https://app.example.com/invite?token=inv1"));
        assert!(msg.text_body.contains("24 hours"));
    }

    #[test]
    fn test_team_invite_email_renders() {
        let msg = team_invite_email(
            "member@example.com",
            "https://app.example.com/team-invite?token=t1",
            "My Workspace",
            "Alice",
            "Ursnip",
        );
        assert_eq!(msg.to, "member@example.com");
        assert!(msg.html_body.contains("Alice"));
        assert!(msg.html_body.contains("My Workspace"));
        assert!(msg.text_body.contains("7 days"));
        assert!(msg.subject.contains("My Workspace"));
    }
}
