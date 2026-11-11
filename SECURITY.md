# Security policy

nirvor is a DNS resolver meant to face subscriber networks, so security reports are taken seriously.

## Reporting a vulnerability

Please **don't open a public issue** for security problems. Use GitHub's private reporting instead: [Report a vulnerability](https://github.com/Muktaditbf/nirvor/security/advisories/new).

Include:

- what you found;
- how to reproduce it (a packet capture or a `dig` command helps);
- the impact you expect, such as cache poisoning, amplification, crash, auth bypass or open-resolver behaviour.

You can expect a first reply within 7 days.

## Scope

In scope:

- the DNS listeners and query pipeline;
- the cache;
- root zone verification;
- the ACL, rate limiting and RRL;
- the dashboard's authentication, sessions and CSRF protection;
- the HTTP API.

Out of scope:

- denial of service from queries the ACL already allows, at rates above the configured limits;
- issues that need the admin password.

## Supported versions

Only the latest release and `main` are supported.
