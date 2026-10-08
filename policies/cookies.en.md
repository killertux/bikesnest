This Cookie Policy describes browser cookies, local preferences and provider telemetry used with BikesNest. It complements the [Privacy Policy](/privacy).

## What we use

The application uses first-party cookies for sign-in, form protection and language preference. Local preferences and external provider processing are described separately below.

| Cookie | Purpose | Duration | Type | Attributes |
|---|---|---|---|---|
| `session_id` | Keeps you signed in after login | 30 days idle (at most 90 days) | Necessary | HttpOnly, Secure, SameSite=Lax |
| `csrf` | Protects forms against cross-site request forgery (CSRF) while you are not signed in | 1 hour | Necessary — security | HttpOnly, SameSite=Lax |
| `lang` | Remembers the language you chose (Portuguese or English); only set when you switch language | 1 year | Functional | SameSite=Lax |

The map-open preference is stored in *localStorage* under `bn.search.mapOpen`. It remembers whether you opened or closed the search map, has no application-set expiry, and changes when you toggle that preference. You can remove it by clearing this site's storage in your browser; your browser may also remove it. The application does not send this preference as a cookie. Blocking local storage does not prevent the map toggle from working, but the preference may not be remembered.

Cloudflare Web Analytics is used for page-view and performance measurements through a browser beacon. These measurements are separate from the local map preference and our sign-in cookies. Cloudflare describes this service as privacy-first; see [Cloudflare Web Analytics](https://developers.cloudflare.com/web-analytics/about/). The presence of analytics does not by itself establish that an analytics cookie is stored.

## Third-party cookies

BikesNest does not use third-party cookies for advertising or tracking. When the map is displayed, your browser contacts the selected map provider directly; that request carries your IP address, and the provider handles its own storage or cookies under its privacy policy. See "Who we share data with" in the [Privacy Policy](/privacy).

## How to control cookies

You can delete or block cookies in your browser settings. Without the `session_id` cookie you cannot stay signed in; without the `csrf` cookie public forms (such as sign-up and login) do not work.

## Changes

We will update this Policy when our browser storage or telemetry practices change. Where applicable law requires consent for optional technologies, we will request it before activating them. Previous versions are at [/cookies/versions](/cookies/versions).

Contact: **{{CONTACT_EMAIL}}**.
