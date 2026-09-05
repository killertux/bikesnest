//! Internationalization: pt-BR + en; strings not hard-coded
//! in domain/application logic).
//!
//! The catalog lives in its own crate because two layers render localized
//! text: the web layer (pages) and the infrastructure layer (transactional
//! emails, rendered from the recipient's stored locale by the job handler that
//! sends them). Infrastructure must not depend on web, so the shared piece —
//! [`Locale`], [`Translator`] and the catalog — sits here, below both.
//!
//! Locale is resolved per request: a `lang` cookie (set by the header toggle
//! via `GET /lang/{code}`) wins; otherwise the `Accept-Language` header is
//! parsed; the fallback is pt-BR. The catalog is a compile-time `match`, so a
//! missing key degrades to the key itself rather than panicking.
//!
//! The axum request extractor is behind the `axum` feature (enabled by the web
//! crate only), so nothing below the web layer pulls axum in to read a string.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locale {
    En,
    PtBr,
}

impl Locale {
    /// Value for `<html lang>` and `hreflang`.
    pub fn html_lang(self) -> &'static str {
        match self {
            Locale::En => "en",
            Locale::PtBr => "pt-BR",
        }
    }

    /// Cookie/route code.
    pub fn code(self) -> &'static str {
        match self {
            Locale::En => "en",
            Locale::PtBr => "pt-br",
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        match code.to_ascii_lowercase().as_str() {
            "en" => Some(Locale::En),
            "pt-br" | "pt" => Some(Locale::PtBr),
            _ => None,
        }
    }
}
/// Request-locale resolution + the axum extractor. Only the web layer enables
/// this feature; the email renderer takes the locale from the user record.
#[cfg(feature = "axum")]
mod request {
    use super::Locale;
    use axum::extract::FromRequestParts;
    use axum::http::request::Parts;

    impl Locale {
        /// Resolve from request headers: `lang` cookie first, then Accept-Language,
        /// then the pt-BR fallback.
        pub fn from_headers(headers: &axum::http::HeaderMap) -> Self {
            if let Some(cookie) = headers
                .get(axum::http::header::COOKIE)
                .and_then(|v| v.to_str().ok())
                && let Some(l) = cookie_lang(cookie)
            {
                return l;
            }
            if let Some(al) = headers
                .get(axum::http::header::ACCEPT_LANGUAGE)
                .and_then(|v| v.to_str().ok())
                && let Some(l) = accept_language(al)
            {
                return l;
            }
            Locale::PtBr
        }
    }

    /// Extract the `lang` cookie value, if present and recognized.
    fn cookie_lang(cookie_header: &str) -> Option<Locale> {
        cookie_header
            .split(';')
            .filter_map(|kv| kv.split_once('='))
            .find(|(k, _)| k.trim() == "lang")
            .and_then(|(_, v)| Locale::from_code(v.trim()))
    }

    /// Parse `Accept-Language`, honoring the first tag whose primary subtag we
    /// support (quality values are ignored — good enough for a two-locale app).
    fn accept_language(header: &str) -> Option<Locale> {
        for part in header.split(',') {
            let tag = part
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            if tag.starts_with("pt") {
                return Some(Locale::PtBr);
            }
            if tag.starts_with("en") {
                return Some(Locale::En);
            }
        }
        None
    }

    impl<S: Send + Sync> FromRequestParts<S> for Locale {
        type Rejection = std::convert::Infallible;

        async fn from_request_parts(
            parts: &mut Parts,
            _state: &S,
        ) -> Result<Self, Self::Rejection> {
            Ok(Locale::from_headers(&parts.headers))
        }
    }
}
/// Request-scoped translator carried into every template and view builder.
#[derive(Debug, Clone, Copy)]
pub struct Translator {
    pub locale: Locale,
}

impl Translator {
    pub fn new(locale: Locale) -> Self {
        Self { locale }
    }

    /// Look up a message by key (Askama-facing).
    pub fn t(&self, key: &str) -> &'static str {
        msg(self.locale, key)
    }

    pub fn lang(&self) -> &'static str {
        self.locale.html_lang()
    }

    pub fn is_en(&self) -> bool {
        self.locale == Locale::En
    }

    pub fn is_pt(&self) -> bool {
        self.locale == Locale::PtBr
    }

    /// Localized "N parking spots" (singular/plural).
    pub fn spots(&self, n: i64) -> String {
        if n == 1 {
            self.t("search.count.one").to_string()
        } else {
            self.t("search.count.other").replace("{n}", &n.to_string())
        }
    }

    /// "Spot {n}" — the accessible name of a numbered result: the badge on a
    /// card and the number drawn in its map marker say the same thing.
    ///
    /// Takes `&usize` because this is called straight from a template, and
    /// Askama passes a struct field to a method call by reference.
    pub fn spot(&self, n: &usize) -> String {
        self.t("map.spot").replace("{n}", &n.to_string())
    }

    /// Localized security-attribute label from its catalog code.
    pub fn security(&self, code: &str) -> &'static str {
        match code {
            "dedicated_locking_point" => self.t("security.dedicated_locking_point"),
            "indoor" => self.t("security.indoor"),
            "cctv" => self.t("security.cctv"),
            "staffed" => self.t("security.staffed"),
            "security_guard" => self.t("security.security_guard"),
            "controlled_access" => self.t("security.controlled_access"),
            "well_lit" => self.t("security.well_lit"),
            "restricted_access" => self.t("security.restricted_access"),
            _ => "",
        }
    }
}

/// The bilingual catalog. Returns `(en, pt-BR)`; unknown keys fall back to the
/// key so a missing translation is visible but never fatal.
pub fn msg(locale: Locale, key: &str) -> &'static str {
    let (en, pt): (&str, &str) = match key {
        // --- nav / brand / footer -----------------------------------------
        "brand.home_aria" => ("BikesNest — home", "BikesNest — início"),
        "a11y.skip" => ("Skip to content", "Pular para o conteúdo"),
        "a11y.primary_nav" => ("Primary", "Navegação principal"),
        "a11y.mobile_nav" => ("Mobile", "Navegação móvel"),
        "a11y.footer_nav" => ("Footer", "Rodapé"),
        "a11y.breadcrumb" => ("Breadcrumb", "Trilha de navegação"),
        "a11y.close" => ("Close", "Fechar"),
        "a11y.report_filter" => ("Report state filter", "Filtro de estado do relato"),
        "a11y.results_pages" => ("Results pages", "Páginas de resultados"),
        "a11y.dialog_photo_title" => ("Photo", "Foto"),
        "nav.how" => ("How it works", "Como funciona"),
        "nav.spots" => ("Parking spots", "Vagas de bike"),
        "nav.community" => ("Community", "Comunidade"),
        "nav.home" => ("Home", "Início"),
        "nav.community_how" => ("Community & how it works", "Comunidade e como funciona"),
        "nav.favorites" => ("Favorites", "Favoritos"),
        "nav.contributions" => ("Contributions", "Contribuições"),
        "nav.moderation" => ("Moderation", "Moderação"),
        "nav.admin" => ("Admin", "Administração"),
        "nav.audit" => ("Audit", "Auditoria"),
        "nav.add_spot" => ("Add a spot", "Adicionar vaga"),
        "nav.verify_to_contribute" => (
            "Verify your email to contribute",
            "Verifique seu e-mail para contribuir",
        ),
        "nav.account_menu_aria" => ("Account menu", "Menu da conta"),
        "nav.signup_to_add" => (
            "Create an account to add a spot",
            "Crie uma conta para adicionar uma vaga",
        ),
        "menu.open" => ("Open menu", "Abrir menu"),
        "lang.group" => ("Language", "Idioma"),
        "lang.pt_aria" => ("Português (Brasil)", "Português (Brasil)"),
        "lang.en_aria" => ("English", "English"),
        "auth.login" => ("Log in", "Entrar"),
        "auth.signup" => ("Sign up", "Criar conta"),
        "footer.tagline" => (
            "A community-maintained map of bicycle parking, built by and for cyclists.",
            "Um mapa de bicicletários mantido pela comunidade, feito por e para quem pedala.",
        ),
        "footer.about" => ("About", "Sobre"),
        "footer.privacy" => ("Privacy policy", "Política de privacidade"),
        "footer.terms" => ("Terms of service", "Termos de uso"),
        "footer.cookies" => ("Cookie policy", "Política de cookies"),
        "footer.coming" => (
            "Coming in a later milestone",
            "Em breve, em uma próxima etapa",
        ),
        "footer.subtitle" => (
            "Community-maintained bicycle parking",
            "Bicicletários mantidos pela comunidade",
        ),

        // --- home (P1) -----------------------------------------------------
        "home.title" => (
            "BikesNest — find bicycle parking you can trust",
            "BikesNest — encontre bicicletários confiáveis",
        ),
        "home.eyebrow" => (
            "Community-powered bike parking",
            "Bicicletários feitos pela comunidade",
        ),
        "home.hero.title" => (
            "A place to park. More room to explore.",
            "Um lugar para a bike. Mais cidade para você.",
        ),
        "home.hero.subtitle" => (
            "Search any address in Curitiba and see nearby bicycle parking — with cost, security and how recently each spot was checked.",
            "Busque qualquer endereço em Curitiba e veja bicicletários por perto — com custo, segurança e há quanto tempo cada vaga foi conferida.",
        ),
        "home.search.placeholder" => ("Where are you going?", "Para onde você vai?"),
        "home.search.button" => ("Search parking", "Buscar vagas"),
        "home.search.locate" => ("Use my location", "Usar minha localização"),
        "home.locate_denied" => (
            "We couldn't get your location. Type a destination instead.",
            "Não conseguimos obter sua localização. Digite um destino.",
        ),
        "home.search.locate_hint" => (
            "Used once, only for this search.",
            "Usada uma vez, só para esta busca.",
        ),
        "home.how.title" => ("How it works", "Como funciona"),
        "home.how.heading" => (
            "From destination to parked bike",
            "Do destino à bike estacionada",
        ),
        "home.how.s1.title" => ("Search a destination", "Busque um destino"),
        "home.how.s1.body" => (
            "Type where you are headed. We find bicycle parking within walking distance.",
            "Digite para onde você vai. Encontramos bicicletários a uma curta caminhada.",
        ),
        "home.how.s2.title" => ("Compare the spots", "Compare as vagas"),
        "home.how.s2.body" => (
            "Cost, security features, opening hours and how fresh the information is — side by side.",
            "Custo, itens de segurança, horários e quão atual é a informação — lado a lado.",
        ),
        "home.how.s3.title" => ("Park with confidence", "Estacione com confiança"),
        "home.how.s3.body" => (
            "Pick a spot, navigate there, and help keep the map honest for the next rider.",
            "Escolha uma vaga, siga até lá e ajude a manter o mapa confiável para quem vem depois.",
        ),
        "home.featured.title" => (
            "Recently added near Rua XV",
            "Adicionados recentemente perto da Rua XV",
        ),
        "home.featured.link" => ("See all parking", "Ver todas as vagas"),
        "home.explore_center" => ("Explore spots downtown", "Explorar vagas no centro"),
        "home.community.eyebrow" => (
            "A map kept honest by riders",
            "Um mapa mantido honesto por quem pedala",
        ),
        "home.community.title" => (
            "Every good parking spot is known by someone who rides past it",
            "Toda boa vaga é conhecida por alguém que passa por ela",
        ),
        "home.community.body" => (
            "BikesNest grows from real riders adding spots, confirming they still exist, and flagging what changed. No single source — just the people who park there.",
            "O BikesNest cresce com ciclistas reais adicionando vagas, confirmando que ainda existem e sinalizando o que mudou. Sem fonte única — só quem estaciona ali.",
        ),
        "home.community.p1" => (
            "Anyone can add a spot",
            "Qualquer pessoa pode adicionar uma vaga",
        ),
        "home.community.p2" => (
            "Riders confirm and correct details",
            "Quem pedala confirma e corrige detalhes",
        ),
        "home.community.p3" => (
            "Freshness shows how current it is",
            "A atualidade mostra o quão recente é",
        ),
        "home.community.link" => ("Learn how contributing works", "Veja como contribuir"),
        "home.cta.title" => ("Find your next parking spot", "Encontre sua próxima vaga"),
        "home.cta.body" => (
            "Start with a destination — no account needed.",
            "Comece por um destino — sem precisar de conta.",
        ),
        "home.cta.button" => ("Search parking", "Buscar vagas"),

        // --- search (P2) ---------------------------------------------------
        // Frontend-only listing collaboration concept.
        "collab.preview" => ("Interactive preview", "Prévia interativa"),
        "collab.preview_body" => (
            "Sample proposals and history. Nothing here is saved.",
            "Propostas e histórico de exemplo. Nada aqui é salvo.",
        ),
        "collab.reset" => ("Reset preview", "Reiniciar prévia"),
        "collab.published" => (
            "You’re viewing the published listing",
            "Você está vendo a versão publicada",
        ),
        "collab.photos" => ("Suggest photos", "Sugerir fotos"),
        "collab.suggest" => ("Suggest changes", "Sugerir alterações"),
        "collab.pending_label" => ("proposals to review", "propostas para avaliar"),
        "collab.pending_body" => (
            "The current information stays published while the community reviews changes.",
            "A informação atual continua publicada enquanto a comunidade avalia as alterações.",
        ),
        "collab.review" => ("Review proposals", "Avaliar propostas"),
        "collab.tabs" => ("Listing views", "Visualizações da vaga"),
        "collab.overview" => ("Overview", "Visão geral"),
        "collab.proposals" => ("Proposals", "Propostas"),
        "collab.history" => ("History", "Histórico"),
        "collab.community" => ("Made better together", "Melhor com a comunidade"),
        "collab.help_title" => ("Know this spot?", "Conhece esta vaga?"),
        "collab.help_body" => (
            "A small correction can make the next rider’s trip easier.",
            "Uma pequena correção pode facilitar a próxima pedalada.",
        ),
        "collab.photos_help" => (
            "A recent photo helps someone know what to expect.",
            "Uma foto recente ajuda quem chega a saber o que esperar.",
        ),
        "collab.review_heading" => (
            "Help keep this listing accurate",
            "Ajude a manter esta vaga atualizada",
        ),
        "collab.rules" => (
            "Non-photo changes are accepted at 6 approvals. A moderator can decide sooner. Rejections are shown separately.",
            "Alterações sem fotos são aceitas com 6 aprovações. A moderação pode decidir antes. Rejeições são contadas separadamente.",
        ),
        "collab.new_proposal" => ("Propose another change", "Propor outra alteração"),
        "collab.demo_tools" => ("Preview controls", "Controles da prévia"),
        "collab.demo_tools_body" => (
            "Try moderator decisions below. These controls only simulate the future workflow.",
            "Experimente as decisões de moderação abaixo. Estes controles apenas simulam o fluxo futuro.",
        ),
        "collab.moderator_preview" => (
            "Show moderator actions (simulation)",
            "Mostrar ações de moderação (simulação)",
        ),
        "collab.history_eyebrow" => (
            "An open record of improvements",
            "Um registro das melhorias",
        ),
        "collab.history_heading" => ("Every version has a story", "Cada versão tem uma história"),
        "collab.history_body" => (
            "Compare previous versions, proposal decisions and vote totals. These are example records, not the real listing history.",
            "Compare versões anteriores, decisões e totais de votos. São registros de exemplo, não o histórico real desta vaga.",
        ),
        "collab.privacy_title" => (
            "Transparency without exposing people",
            "Transparência sem expor pessoas",
        ),
        "collab.privacy_body" => (
            "Only vote totals are public. Authors appear as anonymous unless they choose to show a public name. This preview uses no real contributor identities.",
            "Apenas os totais de votos são públicos. Autores aparecem como anônimos, a menos que escolham exibir um nome público. Esta prévia não usa identidades reais.",
        ),
        "collab.editor_help" => (
            "Change only what needs updating. You’ll review the differences before sharing.",
            "Altere apenas o que precisa ser atualizado. Você revisará as diferenças antes de compartilhar.",
        ),
        "collab.basics" => ("About this spot", "Sobre esta vaga"),
        "collab.field_title" => ("Listing title", "Nome da vaga"),
        "collab.field_description" => ("Description", "Descrição"),
        "collab.location" => ("Location", "Localização"),
        "collab.address" => ("Street address", "Endereço"),
        "collab.latitude" => ("Latitude", "Latitude"),
        "collab.longitude" => ("Longitude", "Longitude"),
        "collab.location_help" => (
            "Adjust the address and coordinates to suggest moving the pin.",
            "Ajuste o endereço e as coordenadas para sugerir a mudança do ponto.",
        ),
        "collab.photo_rule" => (
            "Photo additions and removals always need moderator approval, regardless of votes.",
            "Inclusões e remoções de fotos sempre precisam da aprovação da moderação, independentemente dos votos.",
        ),
        "collab.choose_photos" => ("Choose photos to add", "Escolher fotos para adicionar"),
        "collab.upload_hint" => (
            "Up to 5 JPEG, PNG or WebP images, 10 MB each. Local preview only.",
            "Até 5 imagens JPEG, PNG ou WebP, de até 10 MB cada. Apenas prévia local.",
        ),
        "collab.remove_help" => (
            "Select any published photo you think should be removed. It stays visible until a moderator accepts.",
            "Selecione as fotos publicadas que devem ser removidas. Elas continuam visíveis até a aprovação da moderação.",
        ),
        "collab.remove_photo" => ("Request removal", "Solicitar remoção"),
        "collab.other" => ("Other details", "Outras informações"),
        "collab.other_help" => (
            "Capacity, access restrictions, or anything else we should update",
            "Capacidade, restrições de acesso ou outra informação a atualizar",
        ),
        "collab.reason" => (
            "Why are you suggesting this?",
            "Por que você sugere esta alteração?",
        ),
        "collab.reason_placeholder" => (
            "Tell the community what you noticed…",
            "Conte à comunidade o que você observou…",
        ),
        "collab.review_draft" => (
            "Check your changes. Photos are submitted separately so the other details can be approved by the community.",
            "Confira suas alterações. Fotos são enviadas separadamente para que os outros detalhes possam ser aprovados pela comunidade.",
        ),
        "collab.back" => ("Back to editing", "Voltar à edição"),
        "collab.preview_changes" => ("Review changes", "Revisar alterações"),
        "collab.before" => ("Published now", "Publicado agora"),
        "collab.after" => ("Proposed", "Proposto"),
        "collab.pending" => ("Under review", "Em avaliação"),
        "collab.accepted" => ("Accepted in preview", "Aceita na prévia"),
        "collab.rejected" => ("Rejected in preview", "Rejeitada na prévia"),
        "collab.proposed_by" => ("Suggested by", "Sugerida por"),
        "collab.rider" => ("Cyclist", "Ciclista"),
        "collab.you" => ("You", "Você"),
        "collab.sample_time" => ("Example record", "Registro de exemplo"),
        "collab.now" => ("This preview", "Nesta prévia"),
        "collab.sample_title" => (
            "Update price and opening hours",
            "Atualizar preço e horário",
        ),
        "collab.sample_reason" => (
            "The sign at the entrance lists a fee and new opening hours. I checked during my last visit.",
            "A placa na entrada informa uma tarifa e novos horários. Conferi na minha última visita.",
        ),
        "collab.sample_photo_title" => (
            "Add a clearer photo of the bike rack",
            "Adicionar uma foto mais clara do paraciclo",
        ),
        "collab.sample_photo_reason" => (
            "An example photo suggestion to show how image review works.",
            "Uma sugestão de foto de exemplo para demonstrar como funciona a avaliação de imagens.",
        ),
        "collab.your_proposal" => (
            "Suggested listing update",
            "Atualização sugerida para a vaga",
        ),
        "collab.approve" => ("Approve", "Aprovar"),
        "collab.reject" => ("Reject", "Rejeitar"),
        "collab.votes" => ("Vote totals", "Totais de votos"),
        "collab.approvals" => ("approvals", "aprovações"),
        "collab.rejections" => ("rejections", "rejeições"),
        "collab.alternative" => ("Suggest an alternative", "Sugerir uma alternativa"),
        "collab.vote_help" => (
            "One vote per person. Click again to withdraw, or choose the other option.",
            "Um voto por pessoa. Clique novamente para retirar ou escolha a outra opção.",
        ),
        "collab.progress" => ("approvals to accept", "aprovações para aceitar"),
        "collab.compare" => ("See what changes", "Ver o que muda"),
        "collab.peer" => (
            "Simulate another person’s approval",
            "Simular aprovação de outra pessoa",
        ),
        "collab.moderator" => ("Moderator simulation", "Simulação de moderação"),
        "collab.mod_approve" => ("Accept now", "Aceitar agora"),
        "collab.mod_reject" => ("Reject now", "Rejeitar agora"),
        "collab.submit" => (
            "Share proposal in preview",
            "Compartilhar proposta na prévia",
        ),
        "collab.no_changes" => (
            "Change at least one detail or choose a photo first.",
            "Altere ao menos uma informação ou escolha uma foto.",
        ),
        "collab.saved" => (
            "Your proposal is visible in this preview. The published listing hasn’t changed.",
            "Sua proposta está visível nesta prévia. A versão publicada não foi alterada.",
        ),
        "collab.voted" => ("Your vote was updated.", "Seu voto foi atualizado."),
        "collab.decided" => (
            "Decision recorded in the preview. Open History to inspect the result; the real listing is unchanged.",
            "Decisão registrada na prévia. Abra o Histórico para ver o resultado; a vaga real não foi alterada.",
        ),
        "collab.photo_add" => ("Add photo", "Adicionar foto"),
        "collab.unset" => ("Not provided", "Não informado"),
        "collab.invalid_photo" => (
            "Choose up to 5 JPEG, PNG or WebP photos, no larger than 10 MB each.",
            "Escolha até 5 fotos JPEG, PNG ou WebP, de no máximo 10 MB cada.",
        ),
        "collab.version" => ("Version", "Versão"),
        "collab.snapshot" => (
            "View this version’s full details",
            "Ver todos os detalhes desta versão",
        ),
        "collab.original" => (
            "Published version · example history",
            "Versão publicada · histórico de exemplo",
        ),
        "collab.history_reason" => (
            "The first version of this listing, shown here as an example.",
            "A primeira versão desta vaga, exibida aqui como exemplo.",
        ),
        "collab.community_accepted" => (
            "Accepted by the community after 6 approvals.",
            "Aceita pela comunidade após 6 aprovações.",
        ),
        "collab.mod_accepted" => (
            "Accepted by a moderator without waiting for the vote threshold.",
            "Aceita pela moderação sem aguardar o mínimo de votos.",
        ),
        "collab.sample_price" => ("R$ 5 / hour", "R$ 5 / hora"),
        "collab.sample_hours" => ("08:00–20:00", "08:00–20:00"),
        "collab.anonymous" => ("Anonymous cyclist", "Ciclista anônimo"),
        "account.attribution.label" => (
            "Show my display name on new reviews and proposals",
            "Mostrar meu nome em novas avaliações e propostas",
        ),
        "account.attribution.help" => (
            "Off by default. Your display name will be visible to everyone on new contributions only; old anonymous contributions stay anonymous, even if edited. Turning this off removes your name from existing contributions and does not restore it if enabled again. If you have no display name, you remain anonymous. Voter identities are never shown.",
            "Desativado por padrão. Seu nome ficará visível para todos apenas em novas contribuições; contribuições antigas anônimas continuam anônimas, mesmo se editadas. Desativar remove seu nome das contribuições existentes, sem restaurá-lo se você ativar novamente. Sem um nome definido, você continua anônimo. A identidade de quem votou nunca é exibida.",
        ),
        "account.attribution.save" => (
            "Save privacy preference",
            "Salvar preferência de privacidade",
        ),
        "account.attribution.saved" => (
            "Your public-name preference was saved.",
            "Sua preferência de nome público foi salva.",
        ),
        "collab.sample_name" => ("Marina (example name)", "Marina (nome de exemplo)"),
        "collab.name_optin" => (
            "Show my public name on this proposal (preview)",
            "Mostrar meu nome público nesta proposta (prévia)",
        ),
        "collab.name_help" => (
            "Off by default. Uses an example name; voter identities are never displayed.",
            "Desativado por padrão. Usa um nome de exemplo; a identidade de quem vota nunca é exibida.",
        ),
        "collab.own_vote" => (
            "You can’t vote on your own proposal. Other people can review it.",
            "Você não pode votar na própria proposta. Outras pessoas podem avaliá-la.",
        ),
        "search.title" => ("Search — BikesNest", "Busca — BikesNest"),
        "search.eyebrow" => ("Find your next stop", "Encontre sua próxima parada"),
        "search.subtitle" => (
            "Find a spot near your destination. Compare the details and choose where to park.",
            "Encontre uma vaga perto do seu destino. Compare os detalhes e escolha onde parar.",
        ),
        "search.heading.near" => ("Parking near", "Vagas perto de"),
        "search.heading.generic" => ("Nearby parking", "Vagas por perto"),
        "search.count.one" => ("1 parking spot", "1 vaga"),
        "search.count.other" => ("{n} parking spots", "{n} vagas"),
        "search.map.show" => ("Show map", "Mostrar mapa"),
        "search.map.hide" => ("Show list", "Mostrar lista"),
        "search.map.recenter" => ("Recenter", "Recentralizar"),
        "search.map.title" => ("Map", "Mapa"),
        "search.map.pins" => (
            "Numbered pins match the list",
            "Os pinos numerados batem com a lista",
        ),
        "map.destination" => ("Destination", "Destino"),
        "map.on_map" => ("{n} on map", "{n} no mapa"),
        // `{n}` is substituted by `Translator::spot` (card badge + marker).
        "map.spot" => ("Spot {n}", "Vaga {n}"),
        // `{n}` is substituted in search.js, for a cluster marker's label.
        "map.cluster" => ("{n} spots here", "{n} vagas aqui"),
        // --- browse mode (?bbox=) -----------------------------------------
        "search.browse.heading" => ("Parking in this area", "Vagas nesta área"),
        "search.browse.here" => ("Search this area", "Buscar nesta área"),
        "search.browse.explore" => ("Explore the map", "Explorar o mapa"),
        "search.browse.from_center" => (
            "Distances are measured from the centre of the map.",
            "As distâncias são medidas a partir do centro do mapa.",
        ),
        "search.browse.refine" => (
            "Only the ones nearest the centre are listed. Zoom in or pan to a smaller area to see the rest.",
            "A lista mostra apenas as mais próximas do centro. Aproxime o zoom ou mova o mapa para uma área menor para ver o resto.",
        ),
        "search.browse.invalid" => (
            "That map area can't be searched. Move or zoom the map and search the area again.",
            "Não é possível buscar nessa área do mapa. Mova ou aproxime o mapa e busque na área de novo.",
        ),
        "search.browse.no_pages" => (
            "Browsing the map has no pages — move or zoom the map and search the area again.",
            "A navegação pelo mapa não tem páginas — mova ou aproxime o mapa e busque na área de novo.",
        ),
        "search.sort.label" => ("Sort", "Ordenar"),
        "search.sort.recommended" => ("Recommended", "Recomendados"),
        "search.sort.distance" => ("Distance", "Distância"),
        "search.sort.security" => ("Security", "Segurança"),
        "search.sort.rating" => ("Rating", "Avaliação"),
        "search.sort.recently_verified" => ("Recently verified", "Verificados recentemente"),
        "search.filters.button" => ("Filters", "Filtros"),
        "search.filters.results" => ("Filter results", "Filtrar resultados"),
        "search.filters.clear" => ("Clear all", "Limpar tudo"),
        "search.filters.security" => ("Security features", "Itens de segurança"),
        "search.filters.cost" => ("Cost", "Custo"),
        "search.filters.cost.any" => ("Any", "Qualquer"),
        "search.filters.cost.free" => ("Free", "Grátis"),
        "search.filters.cost.paid" => ("Paid", "Pago"),
        "search.filters.cost.unknown" => ("Unknown", "Não informado"),
        "search.filters.type" => ("Type", "Tipo"),
        "search.filters.radius" => ("Radius", "Raio"),
        "search.filters.open_now" => ("Open now", "Aberto agora"),
        "search.filters.apply" => ("Apply filters", "Aplicar filtros"),
        "search.radius.default" => ("default", "padrão"),
        "search.radius.5km" => ("5 km", "5 km"),
        "search.radius.hint" => (
            "Selections apply immediately and stay in the URL.",
            "As seleções valem na hora e ficam na URL.",
        ),
        "search.updating" => ("Updating results…", "Atualizando resultados…"),
        "search.results_aria" => ("Parking results", "Resultados de vagas"),
        "search.empty.title" => ("No parking here yet", "Nenhuma vaga por aqui ainda"),
        "search.empty.body" => (
            "Try a wider radius or a different destination.",
            "Tente um raio maior ou outro destino.",
        ),
        "search.missing" => (
            "Type a destination (or use your location) to find parking nearby.",
            "Digite um destino (ou use sua localização) para encontrar vagas por perto.",
        ),
        "search.geocode_unavailable" => (
            "The location service is temporarily unavailable. Try again in a moment.",
            "O serviço de localização está temporariamente indisponível. Tente novamente em instantes.",
        ),
        "search.geocode_limited" => (
            "Too many searches from your network right now. Try again in a few minutes.",
            "Muitas buscas da sua rede agora. Tente de novo em alguns minutos.",
        ),
        "search.next" => ("Next page", "Próxima página"),

        // --- parking card --------------------------------------------------
        "card.security_unknown" => ("Security unknown", "Segurança não informada"),
        "card.view" => ("View details", "Ver detalhes"),
        "card.no_photo" => ("No photo yet", "Sem foto ainda"),
        "card.until" => ("until", "até"),

        // --- computed labels: type ----------------------------------------
        "type.rack" => ("Bike rack", "Paraciclo"),
        "type.parking_facility" => ("Bicycle parking", "Bicicletário"),
        "type.indoor" => ("Indoor bicycle parking", "Bicicletário coberto"),
        "type.secured" => ("Secured bicycle parking", "Bicicletário vigiado"),
        "type.locker" => ("Bicycle locker", "Armário para bike"),
        "type.other" => ("Other", "Outro"),

        // --- computed labels: cost ----------------------------------------
        "cost.free" => ("Free", "Grátis"),
        "cost.unknown" => ("Cost unknown", "Custo não informado"),
        "cost.paid_unknown" => ("Paid — price unknown", "Pago — preço não informado"),
        "unit.hour" => ("hour", "hora"),
        "unit.day" => ("day", "dia"),
        "unit.month" => ("month", "mês"),
        "unit.entry" => ("entry", "entrada"),

        // --- computed labels: rating / freshness / open -------------------
        "rating.none" => ("No reviews yet", "Sem avaliações ainda"),
        "freshness.fresh" => ("Fresh", "Atual"),
        "freshness.recently_verified" => ("Recently verified", "Verificado há pouco"),
        "freshness.aging" => ("Aging", "Envelhecendo"),
        "freshness.stale" => ("Stale", "Desatualizado"),
        "freshness.very_stale" => ("Very stale", "Muito desatualizado"),
        "freshness.never" => ("Never verified", "Nunca verificado"),
        "open.now" => ("Open now", "Aberto agora"),
        "open.closed" => ("Closed", "Fechado"),
        "open.unknown" => ("Hours unknown", "Horário não informado"),

        // --- computed labels: security codes ------------------------------
        "security.dedicated_locking_point" => ("Dedicated locking point", "Ponto de fixação"),
        "security.indoor" => ("Indoor", "Coberto"),
        "security.cctv" => ("CCTV", "Câmeras"),
        "security.staffed" => ("Staffed", "Com funcionário"),
        "security.security_guard" => ("Security guard", "Segurança"),
        "security.controlled_access" => ("Controlled access", "Acesso controlado"),
        "security.well_lit" => ("Well lit", "Bem iluminado"),
        "security.restricted_access" => ("Restricted access", "Acesso restrito"),

        // --- computed labels: hours + days --------------------------------
        "hours.unknown" => ("Unknown", "Não informado"),
        "hours.closed" => ("Closed", "Fechado"),
        "hours.all_day" => ("Open 24 hours", "Aberto 24 horas"),
        "day.mon" => ("Monday", "Segunda"),
        "day.tue" => ("Tuesday", "Terça"),
        "day.wed" => ("Wednesday", "Quarta"),
        "day.thu" => ("Thursday", "Quinta"),
        "day.fri" => ("Friday", "Sexta"),
        "day.sat" => ("Saturday", "Sábado"),
        "day.sun" => ("Sunday", "Domingo"),

        // --- verification labels ------------------------------------------
        "verified.today" => ("Verified today", "Verificado hoje"),
        "verified.yesterday" => ("Verified yesterday", "Verificado ontem"),
        "verified.days_ago" => ("Last verified {n} days ago", "Verificado há {n} dias"),
        "verified.never" => ("Never verified", "Nunca verificado"),

        // --- details (P3) --------------------------------------------------
        "details.breadcrumb.home" => ("Home", "Início"),
        "details.breadcrumb.search" => ("Parking", "Vagas"),
        "details.badge.community" => ("Community verified", "Verificado pela comunidade"),
        "details.navigate.google" => ("Open in Google Maps", "Abrir no Google Maps"),
        "details.navigate.osm" => ("Open in OpenStreetMap", "Abrir no OpenStreetMap"),
        "details.facts.title" => ("Key facts", "Informações principais"),
        "details.facts.cost" => ("Cost", "Custo"),
        "details.facts.security" => ("Security", "Segurança"),
        "details.facts.freshness" => ("Freshness", "Atualidade"),
        "details.facts.rating" => ("Rating", "Avaliação"),
        "details.hours.title" => ("Opening hours", "Horário de funcionamento"),
        "details.hours.tz" => ("Times shown in", "Horários no fuso"),
        "details.security.title" => ("Security attributes", "Itens de segurança"),
        "details.security.yes" => ("Yes", "Sim"),
        "details.security.no" => ("No", "Não"),
        "details.security.unknown" => ("Unknown", "Não informado"),
        "details.gallery.empty" => ("No photos yet", "Sem fotos ainda"),
        "details.gallery.empty_hint" => (
            "Photos help riders recognize a spot. Adding them arrives with community accounts.",
            "Fotos ajudam a reconhecer uma vaga. Enviá-las chega com as contas da comunidade.",
        ),
        "gallery.empty_can_upload" => (
            "Be the first to add a photo",
            "Seja o primeiro a enviar uma foto",
        ),
        // A fixed name on the button itself (WP21 a11y pass): an uploaded
        // photo's own caption is optional, so the thumbnail's accessible name
        // cannot rely on it alone.
        "gallery.view_photo" => ("View photo", "Ver foto"),
        "details.add_nearby" => (
            "Missing a spot around here?",
            "Faltou alguma vaga por aqui?",
        ),
        "details.reviews.title" => ("Reviews", "Avaliações"),
        "details.reviews.empty" => (
            "No reviews yet — reviews arrive with community accounts.",
            "Sem avaliações ainda — as avaliações chegam com as contas da comunidade.",
        ),
        "details.contribute.title" => (
            "Help keep this spot accurate",
            "Ajude a manter esta vaga correta",
        ),
        "details.contribute.body" => (
            "Favoriting, verifying and proposing changes arrive with community accounts.",
            "Favoritar, verificar e propor mudanças chegam com as contas da comunidade.",
        ),

        // --- about (P7) ----------------------------------------------------
        "about.title" => ("About — BikesNest", "Sobre — BikesNest"),
        "about.hero.eyebrow" => ("The community model", "O modelo da comunidade"),
        "about.hero.title" => (
            "Every good parking spot, known by someone who already rides past it",
            "Toda boa vaga, conhecida por alguém que já passa por ela",
        ),
        "about.hero.body" => (
            "BikesNest is a community-maintained map of bicycle parking. There is no official dataset — the map is only as good, and as current, as the riders who build it.",
            "O BikesNest é um mapa de bicicletários mantido pela comunidade. Não há base oficial — o mapa é tão bom, e tão atual, quanto quem pedala e o constrói.",
        ),
        "about.hero.cta_search" => ("Search parking", "Buscar vagas"),
        "about.how.title" => (
            "A loop that runs on riders",
            "Um ciclo movido por quem pedala",
        ),
        "about.how.s1.title" => ("Someone adds a spot", "Alguém adiciona uma vaga"),
        "about.how.s1.body" => (
            "A rider marks where parking exists, with its type, cost, hours and security.",
            "Um ciclista marca onde há vaga, com tipo, custo, horário e segurança.",
        ),
        "about.how.s2.title" => ("Others confirm it", "Outros confirmam"),
        "about.how.s2.body" => (
            "Riders verify a spot still exists and correct anything that changed.",
            "Quem passa por ali verifica que a vaga existe e corrige o que mudou.",
        ),
        "about.how.s3.title" => ("Everyone sees how fresh it is", "Todos veem quão atual é"),
        "about.how.s3.body" => (
            "Each spot shows when it was last confirmed, so you know how much to trust it.",
            "Cada vaga mostra quando foi confirmada, para você saber o quanto confiar.",
        ),
        "about.fresh.title" => (
            "How verification and freshness work",
            "Como funcionam verificação e atualidade",
        ),
        "about.fresh.body" => (
            "Every spot carries a freshness signal based on when it was last verified.",
            "Cada vaga carrega um sinal de atualidade com base na última verificação.",
        ),
        "about.fresh.col.label" => ("Freshness", "Atualidade"),
        "about.fresh.col.meaning" => ("What it means", "O que significa"),
        "about.fresh.fresh" => (
            "Verified very recently — trust it.",
            "Verificado há muito pouco — pode confiar.",
        ),
        "about.fresh.recently_verified" => (
            "Confirmed lately — likely accurate.",
            "Confirmado recentemente — provavelmente correto.",
        ),
        "about.fresh.aging" => (
            "A while since the last check.",
            "Faz um tempo desde a última conferida.",
        ),
        "about.fresh.stale" => ("Overdue for a re-check.", "Passou da hora de reconferir."),
        "about.fresh.very_stale" => (
            "Long unverified — treat with care.",
            "Sem verificação há muito — cuidado.",
        ),
        "about.contribute.title" => ("Four ways to contribute", "Quatro formas de contribuir"),
        "about.contribute.add.title" => ("Add a spot", "Adicionar uma vaga"),
        "about.contribute.add.body" => (
            "Map parking that isn't here yet.",
            "Mapeie vagas que ainda não estão aqui.",
        ),
        "about.contribute.verify.title" => ("Verify a spot", "Verificar uma vaga"),
        "about.contribute.verify.body" => (
            "Confirm a spot still exists as described.",
            "Confirme que a vaga ainda existe como descrito.",
        ),
        "about.contribute.review.title" => ("Review a spot", "Avaliar uma vaga"),
        "about.contribute.review.body" => (
            "Share how good it is to park there.",
            "Conte como é estacionar ali.",
        ),
        "about.contribute.report.title" => ("Report a problem", "Relatar um problema"),
        "about.contribute.report.body" => (
            "Flag a spot that's gone or wrong.",
            "Sinalize uma vaga que sumiu ou está errada.",
        ),
        "about.moderation.title" => (
            "Moderation keeps it trustworthy",
            "A moderação mantém a confiança",
        ),
        "about.moderation.body" => (
            "Photos and reports are reviewed by moderators before they affect the map, and every moderation action is recorded.",
            "Fotos e denúncias passam por moderadores antes de afetarem o mapa, e cada ação de moderação é registrada.",
        ),
        // Replaces the old "these tools arrive as the project grows" copy: the
        // four contribution entry points below are live, not a future promise.
        "about.moderation.tools_live" => (
            "These tools are live today: add, verify, review and report from any spot page.",
            "Essas ferramentas já estão disponíveis: adicione, verifique, avalie e denuncie a partir da página de qualquer vaga.",
        ),
        "about.cta.title" => ("Ready to find parking?", "Pronto para encontrar vagas?"),
        "about.cta.button" => ("Search parking", "Buscar vagas"),

        // --- errors --------------------------------------------------------
        "error.title" => ("Error", "Erro"),
        "error.bad_request" => (
            "We could not read that request. Please try again.",
            "Não conseguimos ler essa requisição. Tente novamente.",
        ),
        "error.login_required" => ("Log in to continue.", "Entre na sua conta para continuar."),
        "error.forbidden" => (
            "You do not have permission to do that.",
            "Você não tem permissão para fazer isso.",
        ),
        "error.csrf" => (
            "Your session expired or this form is stale. Reload the page and try again.",
            "Sua sessão expirou ou este formulário está desatualizado. Recarregue a página e tente de novo.",
        ),
        "error.method_not_allowed" => (
            "That action is not available here.",
            "Essa ação não está disponível aqui.",
        ),
        "error.too_large" => ("That upload is too large.", "Esse envio é grande demais."),
        "error.too_many" => (
            "Too many requests. Try again in a moment.",
            "Muitas requisições. Tente novamente em instantes.",
        ),
        "error.404.title" => ("Page not found", "Página não encontrada"),
        "error.404.body" => (
            "The page you are looking for does not exist.",
            "A página que você procura não existe.",
        ),
        "error.500.title" => ("Something went wrong", "Algo deu errado"),
        "error.500.body" => (
            "Something went wrong on our side. Please try again.",
            "Algo deu errado do nosso lado. Tente novamente.",
        ),
        "error.home" => ("Back to home", "Voltar ao início"),
        "error.search" => ("Search parking", "Buscar vagas"),
        "error.conflict" => (
            "Someone changed this at the same time — please reload and try again.",
            "Alguém alterou isto ao mesmo tempo — recarregue e tente de novo.",
        ),
        "error.unavailable" => (
            "The service is temporarily unavailable. Please try again in a moment.",
            "O serviço está temporariamente indisponível. Tente novamente em instantes.",
        ),

        // --- auth: register / login (A1/A2) -------------------------------
        "auth.register_title" => ("Create your account", "Crie sua conta"),
        "auth.register_subtitle" => (
            "A community account keeps your contributions and the map honest.",
            "Uma conta da comunidade mantém suas contribuições e o mapa honestos.",
        ),
        "auth.login_title" => ("Log in", "Entrar"),
        // Registration legal notice: "… agree to the <Terms> and acknowledge the <Privacy policy>."
        "auth.legal_prefix" => (
            "By creating an account you confirm that you are 18 or older and agree to the",
            "Ao criar uma conta, você confirma ter 18 anos ou mais e concorda com os",
        ),
        "auth.legal_middle" => ("and acknowledge the", "e declara ter lido a"),
        "auth.login_subtitle" => ("Welcome back.", "Bem-vindo de volta."),
        "auth.email" => ("Email", "E-mail"),
        "auth.display_name" => ("Display name (optional)", "Nome de exibição (opcional)"),
        "auth.password" => ("Password", "Senha"),
        "auth.password_hint" => ("At least 8 characters", "No mínimo 8 caracteres"),
        "auth.have_account" => ("Already have an account?", "Já tem uma conta?"),
        "auth.forgot" => ("Forgot password?", "Esqueceu a senha?"),
        "auth.logout" => ("Log out", "Sair"),
        "auth.google" => ("Continue with Google", "Continuar com o Google"),
        "auth.google_soon" => ("Coming soon", "Em breve"),
        "auth.oauth_note" => ("Or", "Ou"),

        // --- auth: verification (A3) ---------------------------------------
        "auth.verify_title" => ("Verify your email", "Verifique seu e-mail"),
        "auth.verify_success" => ("Email verified", "E-mail verificado"),
        "auth.verify_success_body" => (
            "Your account is active. You can now log in and contribute.",
            "Sua conta está ativa. Agora você pode entrar e contribuir.",
        ),
        "auth.verify_invalid" => (
            "Verification link invalid or expired",
            "Link de verificação inválido ou expirado",
        ),
        "auth.resend_hint" => (
            "Enter your email to resend the link:",
            "Digite seu e-mail para reenviar o link:",
        ),
        "auth.resend_link" => (
            "Resend verification email",
            "Reenviar e-mail de verificação",
        ),

        // --- auth: password reset (A4/A5) ---------------------------------
        "auth.reset_title" => ("Reset your password", "Redefinir sua senha"),
        "auth.reset_subtitle" => (
            "Enter your email and we will send a reset link if it exists.",
            "Digite seu e-mail e enviaremos um link de redefinição se ele existir.",
        ),
        "auth.reset_button" => ("Send reset link", "Enviar link de redefinição"),
        "auth.reset_new_title" => ("Set a new password", "Defina uma nova senha"),
        "auth.reset_new_subtitle" => (
            "Choose a strong new password.",
            "Escolha uma nova senha forte.",
        ),
        "password.set_submit" => ("Save new password", "Salvar nova senha"),

        // --- auth: notices / errors ---------------------------------------
        "auth.registered" => (
            "Check your inbox to verify your email, then log in.",
            "Confira seu e-mail para verificar seu endereço e depois entre.",
        ),
        "auth.verified" => (
            "Email verified — you can log in.",
            "E-mail verificado — você pode entrar.",
        ),
        "auth.reset_sent" => (
            "If that address exists, a reset link has been sent.",
            "Se esse endereço existir, um link de redefinição foi enviado.",
        ),
        "auth.resend_sent" => (
            "If that address exists, a verification email has been sent.",
            "Se esse endereço existir, um e-mail de verificação foi enviado.",
        ),
        "auth.oauth_failed" => (
            "Google sign-in failed. Try again.",
            "Falha ao entrar com o Google. Tente novamente.",
        ),
        "auth.error.invalid_credentials" => (
            "Email or password is incorrect.",
            "E-mail ou senha incorretos.",
        ),
        "auth.error.weak_password" => (
            "Password must be at least 8 characters.",
            "A senha deve ter pelo menos 8 caracteres.",
        ),
        "auth.error.invalid_email" => ("That email is not valid.", "Esse e-mail não é válido."),
        "auth.error.rate_limited" => (
            "Too many attempts. Try again later.",
            "Muitas tentativas. Tente novamente mais tarde.",
        ),
        "auth.error.invalid_token" => (
            "That link is invalid or has expired.",
            "Esse link é inválido ou expirou.",
        ),
        "auth.error.last_admin" => (
            "The system must keep at least one admin.",
            "O sistema precisa manter ao menos um admin.",
        ),
        "auth.error.generic" => (
            "Something went wrong. Try again.",
            "Algo deu errado. Tente novamente.",
        ),

        // --- account (C1) --------------------------------------------------
        "account.title" => ("Your account", "Sua conta"),
        "account.nav" => ("Account", "Conta"),
        "account.profile" => ("Profile", "Perfil"),
        "account.display_name" => ("Display name", "Nome de exibição"),
        "account.roles" => ("Roles", "Funções"),
        "account.email_verified" => ("Email verified", "E-mail verificado"),
        "account.verified_yes" => ("Yes", "Sim"),
        "account.verified_no" => ("No", "Não"),
        "account.activity_title" => ("Your activity", "Sua atividade"),
        "account.settings" => ("Security", "Segurança"),
        "account.change_password" => ("Change password", "Alterar senha"),
        "account.change_email" => ("Change email", "Alterar e-mail"),
        "account.back" => ("Back to account", "Voltar à conta"),
        "account.banner_title" => (
            "Verify your email to contribute",
            "Verifique seu e-mail para contribuir",
        ),
        "account.banner_body" => (
            "Your account is active, but you must confirm your email address before adding or editing parking.",
            "Sua conta está ativa, mas você precisa confirmar seu e-mail antes de adicionar ou editar vagas.",
        ),
        "account.pw_changed" => ("Your password was changed.", "Sua senha foi alterada."),
        "account.email_pending" => (
            "A confirmation link was sent to your new email.",
            "Um link de confirmação foi enviado para seu novo e-mail.",
        ),

        // --- change password (C2) / change email (C3) ----------------------
        "account.pw_title" => ("Change password", "Alterar senha"),
        "account.pw_current" => ("Current password", "Senha atual"),
        "account.pw_new" => ("New password", "Nova senha"),
        "account.pw_submit" => ("Update password", "Atualizar senha"),
        "account.email_title" => ("Change email", "Alterar e-mail"),
        "account.email_current" => ("Current email:", "E-mail atual:"),
        "account.email_new" => ("New email", "Novo e-mail"),
        "account.email_submit" => ("Request change", "Solicitar alteração"),

        // --- roles / account state labels ---------------------------------
        "role.user" => ("User", "Usuário"),
        "role.moderator" => ("Moderator", "Moderador"),
        "role.admin" => ("Admin", "Administrador"),
        "account.state.pending" => ("Pending verification", "Aguardando verificação"),
        "account.state.active" => ("Active", "Ativa"),
        "account.state.suspended" => ("Suspended", "Suspensa"),
        "account.state.deleted" => ("Deleted", "Excluída"),
        "account.unverified" => ("Unverified", "Não verificado"),

        // --- admin user management (M5) ------------------------------------
        "admin.users_title" => ("Users", "Usuários"),
        "admin.user" => ("User", "Usuário"),
        "admin.state" => ("State", "Estado"),
        "admin.roles" => ("Roles", "Funções"),
        "admin.actions" => ("Actions", "Ações"),
        "admin.granted" => ("Role granted.", "Função concedida."),
        "admin.revoked" => ("Role revoked.", "Função revogada."),
        "admin.role_error" => (
            "That action could not be completed.",
            "Não foi possível concluir essa ação.",
        ),
        "admin.grant_moderator" => ("+ Moderator", "+ Moderador"),
        "admin.revoke_moderator" => ("− Moderator", "− Moderador"),
        "admin.grant_admin" => ("+ Admin", "+ Admin"),
        "admin.revoke_admin" => ("− Admin", "− Admin"),

        // --- M3 contributions: add (D1) ---------------------------------
        "new.title" => ("Add a parking spot", "Adicionar uma vaga"),
        "new.subtitle" => (
            "Mark where bicycle parking exists — type, cost, hours, security.",
            "Marque onde há bicicletário — tipo, custo, horário, segurança.",
        ),
        "new.added" => ("Added as spot", "Vaga adicionada"),
        "new.duplicate.title" => ("This may already be listed", "Pode já estar cadastrado"),
        "new.duplicate.warning" => (
            "It looks similar to existing listings. You can still keep the new spot, but consider that it may already be mapped.",
            "Parece similar a vagas existentes. Você ainda pode manter a nova, mas considere que talvez já esteja mapeada.",
        ),
        "new.field.name" => ("Name", "Nome"),
        "new.field.address" => ("Address", "Endereço"),
        "new.field.description" => ("Description (optional)", "Descrição (opcional)"),
        "new.field.type" => ("Type", "Tipo"),
        "new.field.cost" => ("Cost", "Custo"),
        "new.field.lat" => ("Latitude", "Latitude"),
        "new.field.lon" => ("Longitude", "Longitude"),
        "new.field.tz" => ("Timezone", "Fuso horário"),
        "new.field.tz_hint" => (
            "Left blank, we derive it from the pin. You can override.",
            "Em branco, o derivamos do ponto. Você pode alterar.",
        ),
        "new.field.security" => ("Security attributes", "Itens de segurança"),
        "new.field.price" => ("Price", "Preço"),
        "new.field.currency" => ("Currency", "Moeda"),
        "new.field.unit" => ("Per", "Por"),
        "new.submit" => ("Add parking spot", "Adicionar vaga"),

        // --- the shared add/edit form: pin picker, hours, tri-state -------
        "form.pin.title" => ("Where is it?", "Onde fica?"),
        "form.pin_hint" => ("Drag the pin onto the spot.", "Arraste o pino até a vaga."),
        "form.picked" => ("Picked position", "Posição escolhida"),
        "form.picked_none" => ("No position picked yet", "Nenhuma posição escolhida"),
        "form.use_location" => ("Use my location", "Usar minha localização"),
        "form.locate_failed" => (
            "We couldn't get your location. Drag the pin instead.",
            "Não conseguimos obter sua localização. Arraste o pino.",
        ),
        "form.geocode_address" => ("Find address", "Localizar endereço"),
        "form.geocode_failed" => (
            "We couldn't find that address. Drag the pin instead.",
            "Não encontramos esse endereço. Arraste o pino.",
        ),
        "form.advanced" => ("Advanced", "Avançado"),
        "form.advanced_hint" => (
            "Exact coordinates and timezone. Left alone, the pin sets the position and we derive the timezone.",
            "Coordenadas exatas e fuso horário. Sem mexer, o pino define a posição e derivamos o fuso.",
        ),
        "form.hours.title" => ("Opening hours", "Horário de funcionamento"),
        "form.hours.hint" => (
            "Pick a state per day. \"Set hours\" takes one or two time ranges; a range that ends before it starts runs past midnight.",
            "Escolha um estado por dia. \"Definir horário\" aceita um ou dois intervalos; um intervalo que termina antes de começar passa da meia-noite.",
        ),
        "form.hours.ranges" => ("Set hours", "Definir horário"),
        "form.hours.open" => ("Opens", "Abre"),
        "form.hours.close" => ("Closes", "Fecha"),
        "form.hours.invalid_range" => (
            "Check this day's opening and closing times.",
            "Confira os horários de abertura e fechamento deste dia.",
        ),
        "form.hours.overlap" => (
            "The two ranges for this day overlap.",
            "Os dois intervalos deste dia se sobrepõem.",
        ),
        "form.copy_hours" => ("Copy to all days", "Copiar para todos os dias"),
        "form.tri.yes" => ("Yes", "Sim"),
        "form.tri.no" => ("No", "Não"),
        "form.tri.unknown" => ("Don't know", "Não sei"),

        // --- the duplicate interstitial, shown BEFORE anything is created --
        "new.confirm.title" => ("Is this the same spot?", "É a mesma vaga?"),
        "new.confirm.body" => (
            "Spots this close with a similar name are usually the same one. Open it to check — nothing has been created yet.",
            "Vagas tão próximas e com nome parecido geralmente são a mesma. Abra para conferir — nada foi criado ainda.",
        ),
        "new.confirm.is_this" => ("That's the one", "É esta"),
        "new.confirm.create_anyway" => (
            "It's a different spot, create it anyway",
            "É outra vaga, criar mesmo assim",
        ),
        "new.confirm.back" => ("Go back and edit", "Voltar e corrigir"),

        // --- M3 contributions: edit (D2) --------------------------------
        "edit.title" => ("Edit parking spot", "Editar vaga"),
        "edit.subtitle" => (
            "Update the details of this spot. Moving the pin or removal are separate actions below.",
            "Atualize os detalhes desta vaga. Mover o ponto ou remover são ações separadas abaixo.",
        ),
        "edit.submit" => ("Save changes", "Salvar alterações"),
        "edit.link" => ("Edit this spot", "Editar esta vaga"),
        "edit.sensitive.title" => ("Sensitive changes", "Mudanças sensíveis"),
        "edit.sensitive.body" => (
            "Moving the pin or removing a spot could mislead other riders, so these are proposed and reviewed.",
            "Mover o ponto ou remover uma vaga pode enganar outros ciclistas, então isso é proposto e revisado.",
        ),
        "edit.move.title" => ("Move the pin", "Mover o ponto"),
        "edit.move.submit" => ("Propose new location", "Propor novo local"),
        "edit.remove.title" => ("Remove / mark gone", "Remover / marcar como sumido"),
        "edit.remove.submit" => ("Propose change", "Propor mudança"),
        "edit.reason" => ("Reason", "Motivo"),

        // --- M3 reviews (D3) -------------------------------------------
        "review.title" => ("Write a review", "Escrever uma avaliação"),
        "review.subtitle" => ("How was it to park here?", "Como foi estacionar aqui?"),
        "review.rating" => ("Rating", "Avaliação"),
        "review.select" => ("Choose a rating", "Escolha uma nota"),
        "review.body" => ("Your review", "Sua avaliação"),
        "review.length_hint" => (
            "Between 1 and 2000 characters.",
            "Entre 1 e 2000 caracteres.",
        ),
        "review.submit" => ("Save review", "Salvar avaliação"),
        "review.write" => ("Write a review", "Escrever uma avaliação"),
        "review.edit" => ("Edit your review", "Editar sua avaliação"),
        "review.error.invalid" => (
            "Rating must be 1 to 5 stars.",
            "A nota deve ser de 1 a 5 estrelas.",
        ),
        "review.error.length" => (
            "Review must be 1 to 2000 characters.",
            "A avaliação deve ter de 1 a 2000 caracteres.",
        ),
        "review.error.generic" => (
            "Could not save your review.",
            "Não foi possível salvar sua avaliação.",
        ),

        // --- M3 favorites (C4) -----------------------------------------
        "favorites.title" => ("Your favorites", "Seus favoritos"),
        "favorites.subtitle" => ("Spots you saved for later.", "Vagas que você salvou."),
        "favorites.save" => ("Save to favorites", "Salvar nos favoritos"),
        "favorites.saved" => ("Saved", "Salvo"),
        "favorites.empty" => ("No favorites yet", "Nenhum favorito ainda"),
        "favorites.empty_hint" => (
            "Tap the heart on a spot to keep it here.",
            "Toque no coração em uma vaga para mantê-la aqui.",
        ),

        // --- M3 contributions history (C5) ------------------------------
        "contrib.title" => ("Your contributions", "Suas contribuições"),
        "contrib.subtitle" => (
            "Everything you have added, verified or reviewed.",
            "Tudo que você adicionou, verificou ou avaliou.",
        ),
        "contrib.empty" => ("No contributions yet", "Nenhuma contribuição ainda"),
        "contrib.empty_hint" => (
            "Add a spot, verify one, or write a review.",
            "Adicione uma vaga, verifique uma ou escreva uma avaliação.",
        ),
        "contrib.kind.added" => ("Added", "Adicionou"),
        "contrib.kind.edited" => ("Edited", "Editou"),
        "contrib.kind.proposed" => ("Proposed", "Propôs"),
        "contrib.kind.reviewed" => ("Reviewed", "Avaliou"),
        "contrib.kind.verified" => ("Verified", "Verificou"),
        "contrib.kind.parked_here" => ("Parked here", "Estacionei aqui"),
        "contrib.kind.favorited" => ("Favorited", "Favoritou"),
        "contrib.kind.photo_pending" => ("Photo awaiting review", "Foto aguardando revisão"),
        "contrib.kind.other" => ("Contributed", "Contribuiu"),
        "contrib.state.active" => ("Active", "Ativa"),
        "contrib.state.pending" => ("Pending", "Pendente"),
        "contrib.state.history" => ("History", "História"),
        "contrib.state.other" => ("—", "—"),

        // --- Confidence ---------------------------------------------
        "confidence.title" => ("Confidence", "Confiança"),
        "confidence.reported" => ("Reported", "Reportado"),
        "confidence.verified" => ("Verified", "Verificado"),
        "confidence.recently_verified" => ("Recently verified", "Verificado há pouco"),
        "confidence.stale" => ("Stale", "Desatualizado"),
        "confidence.conflicting" => ("Conflicting", "Conflitante"),
        "confidence.disputed" => (
            "Some riders say this spot has changed. The map is not averaged — it shows both sides.",
            "Alguns ciclistas dizem que esta vaga mudou. O mapa não faz média — mostra os dois lados.",
        ),
        "confidence.disputes" => ("disputes:", "disputas:"),
        "confidence.parked_here_count" => (
            "Riders who parked here:",
            "Ciclistas que estacionaram aqui:",
        ),

        // --- Verification --------------------------------------------
        "verification.title" => ("Verify this spot", "Verificar esta vaga"),
        "verification.anonymous" => (
            "Log in and verify your email to confirm this spot.",
            "Entre e verifique seu e-mail para confirmar esta vaga.",
        ),
        "verification.saved" => ("Thanks — recorded.", "Obrigado — registrado."),
        "verify.still_exists" => ("Still exists", "Ainda existe"),
        "verify.no_longer_exists" => ("No longer exists", "Não existe mais"),
        "verify.info_changed" => ("Information changed", "Informação mudou"),
        "verify.parked_here" => ("I parked here", "Estacionei aqui"),
        "parked.saved" => (
            "Noted. This helps others spot usage.",
            "Anotado. Isso ajuda a ver o uso.",
        ),

        // --- P3 post-action notices --------------------------------------
        "details.notice.proposed" => (
            "Your change has been submitted and will be reviewed by a moderator before it appears.",
            "Sua mudança foi enviada e será revisada por um moderador antes de aparecer.",
        ),
        "details.notice.edited" => ("Your changes were saved.", "Suas alterações foram salvas."),
        "details.notice.reviewed" => (
            "Thanks — your review was saved.",
            "Obrigado — sua avaliação foi salva.",
        ),
        "details.notice.added" => ("The parking spot was added.", "A vaga foi adicionada."),
        "contribution.created_notice" => (
            "Your spot is live. The community will verify it over time; you can edit it any time.",
            "Sua vaga está no ar. A comunidade vai conferi-la com o tempo; você pode editá-la quando quiser.",
        ),

        // --- P3 recommended because -----------------------------------
        "details.recommend.title" => ("Recommended because", "Recomendado porque"),
        "reason.distance" => ("Close to your destination", "Perto do seu destino"),
        "reason.security" => ("Security attributes", "Itens de segurança"),
        "reason.rating" => ("Rated by riders", "Avaliado por ciclistas"),
        "reason.freshness" => ("Recently verified", "Verificado há pouco"),
        "reason.verification" => ("Confirmed by riders", "Confirmado por ciclistas"),

        // --- Contribution errors -----------------------------------------
        "contribution.error.not_verified" => (
            "Verify your email to contribute.",
            "Verifique seu e-mail para contribuir.",
        ),
        "contribution.error.rate_limited" => (
            "Too many attempts. Try again later.",
            "Muitas tentativas. Tente novamente mais tarde.",
        ),
        "contribution.error.version_conflict" => (
            "Someone else recently changed this. We reloaded the latest values — try again.",
            "Alguém alterou isto recentemente. Recarregamos os valores atuais — tente de novo.",
        ),
        "contribution.error.not_found" => (
            "That spot could not be found.",
            "Não foi possível encontrar essa vaga.",
        ),
        "contribution.error.invalid" => (
            "Some fields are missing or invalid.",
            "Alguns campos estão vazios ou inválidos.",
        ),
        "contribution.error.unauthorized" => (
            "You cannot perform this action.",
            "Você não pode fazer isso.",
        ),
        "contribution.error.timezone" => (
            "Could not determine the timezone from that point.",
            "Não foi possível determinar o fuso a partir do ponto.",
        ),
        "contribution.error.internal" => (
            "Something went wrong. Please try again.",
            "Algo deu errado. Tente novamente.",
        ),
        "contribution.error.not_active" => (
            "This spot is no longer accepting contributions.",
            "Esta vaga não aceita mais contribuições.",
        ),
        "contribution.error.generic" => ("Something went wrong.", "Algo deu errado."),
        "contribution.verify_to" => (
            "Verify your email to confirm this spot.",
            "Verifique seu e-mail para confirmar esta vaga.",
        ),

        // --- time-ago labels ---------------------------------------------
        "time.today" => ("today", "hoje"),
        "time.yesterday" => ("yesterday", "ontem"),
        "time.days_ago" => ("{n} days ago", "há {n} dias"),
        "time.months_ago" => ("{n} months ago", "há {n} meses"),

        // --- attribute codes (verification) ------------------------------
        "attr.name" => ("Name", "Nome"),
        "attr.address" => ("Address", "Endereço"),
        "attr.type" => ("Type", "Tipo"),
        "attr.cost" => ("Cost", "Custo"),
        "attr.hours" => ("Hours", "Horário"),
        "attr.security" => ("Security", "Segurança"),
        "attr.location" => ("Location", "Localização"),
        "attr.unknown" => ("Details", "Detalhes"),

        // --- photos (M4, /) -----------------------------------------
        "photo.upload.title" => ("Add a photo", "Adicionar foto"),
        "photo.upload.hint" => (
            "A photo helps riders recognize a spot. Upload only photos you took of the parking itself and avoid people's faces and licence plates. You are responsible for what you upload; photos are reviewed by moderators and automated tools before they appear.",
            "Uma foto ajuda a reconhecer a vaga. Envie apenas fotos que você tirou do próprio local e evite rostos de pessoas e placas de veículos. Você é responsável pelo que envia; as fotos passam por revisão de moderadores e ferramentas automáticas antes de aparecer.",
        ),
        "photo.upload.submit" => ("Upload photo", "Enviar foto"),
        "photo.upload.pending_notice" => (
            "Photo submitted — it appears once a moderator approves it.",
            "Foto enviada — ela aparece assim que um moderador a aprovar.",
        ),
        "photo.upload.success" => (
            "Photo uploaded. It will appear once approved.",
            "Foto enviada. Ela aparecerá quando for aprovada.",
        ),
        "photo.error.too_large" => (
            "Photo is too large (max 10 MiB).",
            "A foto é muito grande (máx. 10 MiB).",
        ),
        "photo.error.unsupported" => (
            "Unsupported format. Use JPEG, PNG or WebP.",
            "Formato não suportado. Use JPEG, PNG ou WebP.",
        ),
        "photo.error.undecodable" => (
            "That file isn't a readable image.",
            "Esse arquivo não é uma imagem legível.",
        ),
        "photo.error.invalid" => ("Invalid photo input.", "Dados de foto inválidos."),
        "photo.error.too_many_pixels" => (
            "Image resolution is too high (max 20 MP).",
            "A resolução é muito alta (máx. 20 MP).",
        ),
        "photo.error.not_verified" => (
            "Verify your email to add photos.",
            "Verifique seu e-mail para adicionar fotos.",
        ),
        "photo.error.not_found" => ("Location not found.", "Local não encontrado."),
        "photo.error.rate_limited" => (
            "Too many uploads, try again later.",
            "Muitos envios, tente novamente mais tarde.",
        ),
        "photo.error.internal" => (
            "Upload failed. Try a different photo.",
            "Falha no envio. Tente outra foto.",
        ),

        // --- photo moderation queue -----------------------------------
        "moderation.title" => ("Photo moderation", "Moderação de fotos"),
        "moderation.empty" => (
            "No photos awaiting review.",
            "Nenhuma foto aguardando revisão.",
        ),
        "moderation.pending" => ("Pending", "Pendente"),
        "moderation.approve" => ("Approve", "Aprovar"),
        "moderation.reject" => ("Reject", "Rejeitar"),
        "moderation.reason_label" => ("Rejection reason", "Motivo da rejeição"),
        "moderation.reason_placeholder" => (
            "e.g. unclear image, incorrect location",
            "ex.: imagem ilegível, local incorreto",
        ),
        "moderation.approved" => ("Photo approved.", "Foto aprovada."),
        "moderation.rejected" => ("Photo rejected.", "Foto rejeitada."),
        "moderation.reject_missing" => ("Reject (file missing)", "Rejeitar (arquivo ausente)"),
        "moderation.photo_unavailable" => ("Image unavailable", "Imagem indisponível"),
        "moderation.photo_unavailable_hint" => (
            "The file is no longer in storage, so it cannot be reviewed or published. Reject it to clear the queue.",
            "O arquivo não está mais no armazenamento, então não pode ser revisado nem publicado. Rejeite-o para limpar a fila.",
        ),
        "moderation.photo_missing_reason" => (
            "File missing from storage",
            "Arquivo ausente no armazenamento",
        ),
        "moderation.target_gone" => ("(target deleted)", "(alvo excluído)"),
        "moderation.action.hide_review" => ("Hide review", "Ocultar avaliação"),
        "moderation.action.invalidate_parking" => ("Mark invalid", "Marcar como inválida"),
        "moderation.action.hide_photo" => ("Hide photo", "Ocultar foto"),
        "moderation.action.reject_photo" => ("Reject photo", "Rejeitar foto"),
        "moderation.confirm.remove" => (
            "Approve removal of \u{201c}{name}\u{201d}? It will disappear from search and the map.",
            "Aprovar a remoção de \u{201c}{name}\u{201d}? Ela desaparecerá da busca e do mapa.",
        ),
        "moderation.confirm.act_on_content" => (
            "{label} on \u{201c}{name}\u{201d}?",
            "{label} em \u{201c}{name}\u{201d}?",
        ),
        "moderation.err.approve" => ("Approve failed.", "Falha ao aprovar."),
        "moderation.err.reject" => ("Reject failed.", "Falha ao rejeitar."),
        "moderation.error.internal" => ("Moderation action failed.", "Ação de moderação falhou."),
        "moderation.error.stale_proposal" => (
            "This proposal is out of date: the location changed since it was made. Review it again.",
            "Esta proposta está desatualizada: a vaga mudou desde que foi feita. Revise-a novamente.",
        ),
        "moderation.not_found" => ("Photo not found.", "Foto não encontrada."),
        "moderation.not_pending" => (
            "This photo isn't awaiting review.",
            "Esta foto não está aguardando revisão.",
        ),
        "moderation.unauthorized" => (
            "You don't have permission to moderate.",
            "Você não tem permissão para moderar.",
        ),
        "moderation.contributor" => ("Contributor", "Contribuidor"),
        "moderation.exif_note" => (
            "EXIF stripped · processed derivative",
            "EXIF removido · derivado processado",
        ),
        "moderation.locations" => ("Location", "Local"),
        "moderation.dimensions" => ("Dimensions", "Dimensões"),
        "moderation.view" => ("Full size", "Tamanho completo"),
        "moderation.refresh" => ("Refresh queue", "Atualizar fila"),
        "moderation.uploaded" => ("Uploaded", "Enviado"),

        // --- M5 reports + moderation (moderation.rs) -------------------
        "report.title" => ("Report this content", "Denunciar este conteúdo"),
        "report.action" => ("Report", "Denunciar"),
        "report.reason" => ("Reason", "Motivo"),
        "report.description" => ("Details (optional)", "Detalhes (opcional)"),
        "report.submit" => ("Submit report", "Enviar denúncia"),
        "report.cancel" => ("Cancel", "Cancelar"),
        "report.submitted" => (
            "Thanks — your report was submitted.",
            "Obrigado — sua denúncia foi enviada.",
        ),
        "report.claim" => ("Claim", "Assumir"),
        "report.claimed" => ("Report claimed.", "Denúncia assumida."),
        "report.resolve" => ("Resolve", "Resolver"),
        "report.dismiss" => ("Dismiss", "Descartar"),
        "report.resolve_placeholder" => ("Resolution note", "Nota de resolução"),
        "report.resolved_msg" => ("Report resolved.", "Denúncia resolvida."),
        "report.dismissed_msg" => ("Report dismissed.", "Denúncia descartada."),
        "report.claimed.none" => ("Unclaimed", "Sem responsável"),
        "report.reporter.anonymous" => ("Anonymous", "Anônimo"),
        "report.error.invalid_reason" => (
            "That reason is not valid for this content.",
            "Esse motivo não é válido para este conteúdo.",
        ),
        "report.error.duplicate" => (
            "You already reported this. Thanks — a moderator will review it.",
            "Você já denunciou isto. Obrigado — um moderador vai revisar.",
        ),
        "report.error.rate_limited" => (
            "Too many reports. Try again later.",
            "Muitas denúncias. Tente novamente mais tarde.",
        ),
        "report.target.parking" => ("Parking spot", "Vaga"),
        "report.target.parking_photo" => ("Parking photo", "Foto da vaga"),
        "report.target.review" => ("Review", "Avaliação"),
        "report.target.review_photo" => ("Review photo", "Foto da avaliação"),
        "report.target.other" => ("Content", "Conteúdo"),
        "report.reason.nonexistent_parking" => ("Doesn't exist", "Não existe"),
        "report.reason.incorrect_location" => ("Wrong location", "Local errado"),
        "report.reason.incorrect_price" => ("Wrong price", "Preço errado"),
        "report.reason.incorrect_hours" => ("Wrong hours", "Horário errado"),
        "report.reason.incorrect_security" => ("Wrong security info", "Segurança errada"),
        "report.reason.duplicate" => ("Duplicate listing", "Cadastro duplicado"),
        "report.reason.inappropriate_photo" => ("Inappropriate photo", "Foto inadequada"),
        "report.reason.inappropriate_review" => ("Inappropriate review", "Avaliação inadequada"),
        "report.reason.spam" => ("Spam", "Spam"),
        "report.reason.abuse" => ("Abuse", "Abuso"),
        "report.reason.other" => ("Other", "Outro"),
        "report.state.all" => ("All", "Todas"),
        "report.state.open" => ("Open", "Aberta"),
        "report.state.under_review" => ("In review", "Em análise"),
        "report.state.resolved" => ("Resolved", "Resolvida"),
        "report.state.dismissed" => ("Dismissed", "Descartada"),

        // --- moderation dashboard / queues -----------------------------
        "moderation.dashboard.title" => ("Moderation", "Moderação"),
        "moderation.dashboard.subtitle" => (
            "Triage reports, photos and proposals from one place.",
            "Faça a triagem de denúncias, fotos e propostas em um só lugar.",
        ),
        "moderation.dashboard.photos" => ("Pending photos", "Fotos pendentes"),
        "moderation.dashboard.reports" => ("Reports", "Denúncias"),
        "moderation.dashboard.proposals" => ("Pending proposals", "Propostas pendentes"),
        "moderation.dashboard.open" => ("Open", "Abertos"),
        "moderation.dashboard.in_review" => ("In review", "Em análise"),
        "moderation.dashboard.queues" => ("Queues", "Filas"),
        "moderation.dashboard.link.photos" => ("Photo moderation", "Moderação de fotos"),
        "moderation.dashboard.link.reports" => ("Report queue", "Fila de denúncias"),
        "moderation.dashboard.link.proposals" => ("Proposal review", "Revisão de propostas"),
        "moderation.dashboard.link.audit" => ("Audit log", "Registro de auditoria"),
        "moderation.tile.reports_open" => ("Open reports", "Denúncias abertas"),
        "moderation.tile.reports_under_review" => ("Reports in review", "Denúncias em análise"),
        "moderation.tile.photos_pending" => ("Awaiting review", "Aguardando revisão"),
        "moderation.reports.title" => ("Report queue", "Fila de denúncias"),
        "moderation.reports.subtitle" => (
            "Claims are the only open→review move; resolve or dismiss a claim.",
            "Assumir é o único movimento de aberta→em análise; resolva ou descarte.",
        ),
        "moderation.reports.empty" => ("No reports here.", "Nenhuma denúncia aqui."),
        "moderation.proposals.title" => ("Proposal review", "Revisão de propostas"),
        "moderation.proposals.subtitle" => (
            "Approve applies the change with a new revision; reject leaves the listing untouched.",
            "Aprovar aplica a mudança com nova revisão; rejeitar mantém a vaga intacta.",
        ),
        "moderation.proposals.empty" => ("No pending proposals.", "Nenhuma proposta pendente."),
        "moderation.banner" => (
            "This listing is under moderation:",
            "Esta vaga está sob moderação:",
        ),
        "moderation.restore" => ("Restore", "Restaurar"),
        "moderation.photo_hidden" => ("Photo hidden.", "Foto ocultada."),
        "moderation.photo_restored" => ("Photo restored.", "Foto restaurada."),
        "moderation.self_resolve" => (
            "You cannot resolve a report you submitted.",
            "Você não pode resolver uma denúncia que você mesmo enviou.",
        ),
        "moderation.invalid_state" => (
            "That action isn't valid in the current state.",
            "Essa ação não é válida no estado atual.",
        ),
        "moderation.target_not_found" => (
            "The reported content could not be found.",
            "O conteúdo denunciado não foi encontrado.",
        ),
        "moderation.invalid" => ("Invalid input.", "Entrada inválida."),
        "moderation.moderator" => ("Moderator", "Moderador"),
        "moderation.proposer" => ("Proposer", "Proponente"),
        "moderation.actor" => ("Actor", "Ator"),

        // --- proposals ------------------------------------------------
        "proposal.kind.move" => ("Move the pin", "Mover o ponto"),
        "proposal.kind.existence" => ("Change existence", "Mudar existência"),
        "proposal.existence.removed" => ("Removed", "Removida"),
        "proposal.existence.exists" => ("Exists", "Existe"),
        "proposal.approve.lat" => ("Lat", "Lat"),
        "proposal.approve.lon" => ("Lon", "Lon"),
        "proposal.approve.tz" => ("Timezone", "Fuso"),
        "proposal.approve.existence" => ("Existence", "Existência"),
        "proposal.reject_placeholder" => ("Reason to reject", "Motivo da rejeição"),
        "proposal.approved" => ("Proposal approved.", "Proposta aprovada."),
        "proposal.rejected" => ("Proposal rejected.", "Proposta rejeitada."),
        "proposal.reason.label" => ("Proposer's note", "Nota de quem propôs"),
        "proposal.field.coordinates" => ("Coordinates", "Coordenadas"),
        "proposal.field.timezone" => ("Timezone", "Fuso horário"),
        "proposal.field.existence" => ("Existence", "Existência"),
        "proposal.diff.unchanged" => ("unchanged", "sem alteração"),
        "proposal.value.unknown" => ("not set", "não definido"),
        "proposal.map.current" => ("Current position", "Posição atual"),
        "proposal.map.proposed" => ("Proposed position", "Posição proposta"),
        "proposal.map.aria" => (
            "Map showing the current and proposed positions of the pin.",
            "Mapa mostrando as posições atual e proposta do ponto.",
        ),
        "proposal.stale.badge" => ("Out of date", "Desatualizada"),
        "proposal.stale.hint" => (
            "The location changed after this proposal was written, so approving it would overwrite an edit the proposer never saw. Ask for a fresh proposal, or reject it.",
            "A vaga mudou depois que esta proposta foi escrita, então aprová-la sobrescreveria uma edição que quem propôs nunca viu. Peça uma nova proposta ou rejeite-a.",
        ),
        "proposal.manual_review.badge" => ("Needs manual review", "Precisa de revisão manual"),
        "proposal.manual_review.hint" => (
            "This proposal's stored data could not be read. Fill in the values yourself to approve it, or reject it.",
            "Não foi possível ler os dados desta proposta. Preencha os valores manualmente para aprová-la, ou rejeite-a.",
        ),
        "proposal.error.lat" => (
            "Latitude is required and must be between -90 and 90.",
            "A latitude é obrigatória e deve estar entre -90 e 90.",
        ),
        "proposal.error.lon" => (
            "Longitude is required and must be between -180 and 180.",
            "A longitude é obrigatória e deve estar entre -180 e 180.",
        ),
        "proposal.error.timezone" => (
            "A valid IANA timezone is required (e.g. America/Sao_Paulo).",
            "É necessário um fuso horário IANA válido (ex.: America/Sao_Paulo).",
        ),
        "proposal.error.existence" => (
            "Choose whether the location exists or was removed.",
            "Escolha se a vaga existe ou foi removida.",
        ),

        // --- review / parking moderation toasts -----------------------
        "review.hidden" => ("Review hidden.", "Avaliação ocultada."),
        "review.restored" => ("Review restored.", "Avaliação restaurada."),
        "parking.invalidated" => ("Location invalidated.", "Vaga invalidada."),
        "parking.restored" => ("Location restored.", "Vaga restaurada."),
        "review.photos" => ("Photos (optional)", "Fotos (opcional)"),
        "review.photos_hint" => (
            "Only photos of the parking itself, without people's faces or licence plates. You are responsible for what you upload; photos are reviewed before they appear.",
            "Apenas fotos do próprio local, sem rostos de pessoas nem placas de veículos. Você é responsável pelo que envia; as fotos são revisadas antes de aparecer.",
        ),

        // --- admin user management (suspend/restore + contributions) ---
        "admin.suspend" => ("Suspend", "Suspender"),
        "admin.restore" => ("Restore", "Restaurar"),
        "admin.suspended" => (
            "User suspended — sessions revoked.",
            "Usuário suspenso — sessões revogadas.",
        ),
        "admin.restored" => ("User restored to active.", "Usuário restaurado para ativo."),
        "admin.contrib_link" => ("Contributions", "Contribuições"),
        "admin.last_active" => ("Last active", "Última atividade"),
        "admin.contributions" => ("Contributions", "Contribuições"),
        "admin.never" => ("never", "nunca"),
        "admin.users_empty" => (
            "No accounts match this search.",
            "Nenhuma conta corresponde a esta busca.",
        ),
        "admin.search.label" => ("Search accounts", "Buscar contas"),
        "admin.search.placeholder" => ("email or name", "e-mail ou nome"),
        "admin.search.submit" => ("Search", "Buscar"),
        "admin.search.clear" => ("Clear", "Limpar"),
        "admin.email.show" => ("Show", "Mostrar"),
        "admin.email.hide" => ("Hide", "Ocultar"),
        "admin.confirm.suspend" => (
            "Suspend {name}? Every session is revoked immediately.",
            "Suspender {name}? Todas as sessões serão revogadas imediatamente.",
        ),
        "admin.confirm.restore" => ("Restore {name} to active?", "Restaurar {name} para ativo?"),
        "admin.confirm.grant_moderator" => (
            "Grant MODERATOR to {name}? They will be able to hide content and resolve reports.",
            "Conceder MODERATOR a {name}? Poderá ocultar conteúdo e resolver denúncias.",
        ),
        "admin.confirm.revoke_moderator" => (
            "Revoke MODERATOR from {name}?",
            "Revogar MODERATOR de {name}?",
        ),
        "admin.confirm.grant_admin" => (
            "Grant ADMIN to {name}? They will be able to manage every account and role.",
            "Conceder ADMIN a {name}? Poderá gerenciar todas as contas e funções.",
        ),
        "admin.confirm.revoke_admin" => ("Revoke ADMIN from {name}?", "Revogar ADMIN de {name}?"),
        "admin.contrib.title" => ("Contributions", "Contribuições"),
        "admin.contrib.subtitle" => (
            "Inspection view of a user's contribution history.",
            "Visão de inspeção do histórico de contribuições de um usuário.",
        ),
        "admin.audit.title" => ("Audit log", "Registro de auditoria"),
        "admin.audit.subtitle" => (
            "Security, account, role and moderation actions.",
            "Ações de segurança, conta, função e moderação.",
        ),
        "admin.audit.action" => ("Action contains", "Ação contém"),
        "admin.audit.target_type" => ("Target type", "Tipo de alvo"),
        "admin.audit.actor" => ("Actor id", "Id do ator"),
        "admin.audit.from" => ("From", "De"),
        "admin.audit.to" => ("To", "Até"),
        "admin.audit.utc_note" => (
            "Dates are read and shown in UTC, matching how events are stored.",
            "As datas são lidas e exibidas em UTC, como os eventos são armazenados.",
        ),
        "admin.audit.filter" => ("Apply", "Aplicar"),
        "admin.audit.empty" => (
            "No audit events match the filter.",
            "Nenhum evento de auditoria corresponde ao filtro.",
        ),
        "admin.audit.id" => ("Id", "Id"),
        "admin.audit.target" => ("Target", "Alvo"),
        "admin.audit.result" => ("Result", "Resultado"),
        "admin.audit.metadata" => ("Metadata", "Metadados"),
        "admin.audit.when" => ("When", "Quando"),
        "admin.audit.next" => ("Next page", "Próxima página"),
        "pagination.more" => ("Load more", "Carregar mais"),
        "audit.system" => ("System", "Sistema"),
        "audit.result.success" => ("Success", "Sucesso"),
        "audit.result.failure" => ("Failure", "Falha"),

        // --- M6 privacy & account lifecycle ---
        "nav.privacy" => ("Privacy policy", "Política de privacidade"),
        "nav.terms" => ("Terms of service", "Termos de serviço"),
        "nav.cookies" => ("Cookie policy", "Política de cookies"),
        "policy.missing" => (
            "This policy is not available yet.",
            "Esta política ainda não está disponível.",
        ),
        "policy.versions_title" => ("Version history", "Histórico de versões"),
        "policy.effective" => ("Effective", "Em vigor"),
        "policy.version" => ("Version", "Versão"),
        "policy.view_versions" => ("View version history", "Ver histórico de versões"),
        "policy.back" => ("Back to policy", "Voltar à política"),
        "policy.current" => ("Current", "Atual"),
        "policy.superseded" => ("Superseded", "Substituída"),
        "privacy.kind" => ("Kind", "Tipo"),
        "privacy.state" => ("State", "Estado"),
        "privacy.hub_title" => ("Privacy & data", "Privacidade e dados"),
        "privacy.hub_intro" => (
            "Every request on this page is recorded in an auditable trail, and we verify your identity before acting on it.",
            "Cada solicitação desta página é registrada em um histórico auditável e verificamos sua identidade antes de agir.",
        ),
        "privacy.export_title" => ("Export your data", "Exporte seus dados"),
        "privacy.export_desc" => (
            "A machine-readable copy (JSON) of everything we hold about you. Download links expire after 24 hours.",
            "Uma cópia legível por máquina (JSON) de tudo o que temos sobre você. Os links de download expiram em 24 horas.",
        ),
        "privacy.request_export" => ("Request export", "Solicitar exportação"),
        "privacy.request" => ("Request", "Solicitar"),
        "privacy.details" => ("Details (optional)", "Detalhes (opcional)"),
        "privacy.details_placeholder" => (
            "Add anything we should know (optional)",
            "Adicione algo que devemos saber (opcional)",
        ),
        "privacy.rights_title" => (
            "Rectification & other rights",
            "Retificação e outros direitos",
        ),
        "privacy.rights_desc" => (
            "You can also request access, rectification, restriction or objection regarding your data. Most requests are handled automatically; a few need a human pass.",
            "Você também pode solicitar acesso, retificação, restrição ou objeção sobre seus dados. A maioria é tratada automaticamente; algumas precisam de análise humana.",
        ),
        "privacy.rights.rectification_desc" => {
            ("Correct inaccurate data", "Corrigir dados incorretos")
        }
        "privacy.rights.restriction_desc" => (
            "Pause processing of your data",
            "Pausar o processamento dos seus dados",
        ),
        "privacy.rights.objection_desc" => (
            "Stop a specific processing purpose",
            "Interromper uma finalidade específica de processamento",
        ),
        "privacy.rights.consent_desc" => (
            "Withdraw a previously given consent",
            "Revogar um consentimento anteriormente concedido",
        ),
        "privacy.consent_title" => ("Consent records", "Registros de consentimento"),
        "privacy.consent_desc" => (
            "Where we rely on your consent, it is specific, recorded and withdrawable at any time.",
            "Quando usamos seu consentimento, ele é específico, registrado e revogável a qualquer momento.",
        ),
        "privacy.back" => ("Back to Privacy & data", "Voltar a Privacidade e dados"),
        "privacy.kind.access" => ("Access", "Acesso"),
        "privacy.kind.rectification" => ("Rectification", "Retificação"),
        "privacy.kind.deletion" => ("Deletion", "Exclusão"),
        "privacy.kind.export" => ("Export", "Exportação"),
        "privacy.kind.restriction" => ("Restriction", "Restrição"),
        "privacy.kind.objection" => ("Objection", "Objeção"),
        "privacy.kind.consent" => ("Consent withdrawal", "Revogação de consentimento"),
        "privacy.state.open" => ("Open", "Aberto"),
        "privacy.state.in_progress" => ("In progress", "Em andamento"),
        "privacy.state.completed" => ("Completed", "Concluído"),
        "privacy.state.declined" => ("Declined", "Recusado"),
        "export.title" => ("Your data export", "Sua exportação de dados"),
        "export.error" => (
            "Unable to download. The link may have expired or already been used.",
            "Não foi possível baixar. O link pode ter expirado ou já ter sido usado.",
        ),
        "export.none" => (
            "You have not requested an export yet.",
            "Você ainda não solicitou uma exportação.",
        ),
        "export.status_title" => ("Export status", "Status da exportação"),
        "export.requested" => ("Requested", "Solicitada em"),
        "export.expires" => ("Expires", "Expira em"),
        "export.status" => ("Status", "Status"),
        "export.download" => ("Download", "Baixar"),
        "export.expiry_note" => (
            "Links are single-use and expire 24 hours after the export is created; only you can download them while signed in.",
            "Os links são de uso único e expiram 24 horas após a criação da exportação; somente você pode baixá-los enquanto estiver conectado.",
        ),
        "export.state.ready" => ("Ready", "Pronta"),
        "export.state.downloaded" => ("Downloaded", "Baixada"),
        "export.state.expired" => ("Expired", "Expirada"),
        "delete.title" => ("Delete account", "Excluir conta"),
        "delete.intro" => (
            "Your personal identity is removed and your sessions are signed out. Community contributions may be kept, but anonymized — they stop being attributable to you. This can’t be undone.",
            "Sua identidade pessoal é removida e suas sessões são encerradas. As contribuições da comunidade podem ser mantidas, mas anonimizadas — deixam de ser atribuídas a você. Isso não pode ser desfeito.",
        ),
        "delete.request" => ("Request deletion", "Solicitar exclusão"),
        "delete.reauth_error" => (
            "We could not confirm your identity. Check your email and, if you use a password, your current password.",
            "Não foi possível confirmar sua identidade. Verifique seu e-mail e, se você usa senha, sua senha atual.",
        ),
        "delete.last_admin_error" => (
            "You cannot delete your account while you are the only administrator. Promote someone else first.",
            "Você não pode excluir sua conta enquanto for o único administrador. Promova outra pessoa primeiro.",
        ),
        "delete.password_optional" => (
            "blank for Google-only accounts",
            "em branco para contas só Google",
        ),
        "delete.list_identity" => (
            "Your personal identity is removed; you are signed out everywhere.",
            "Sua identidade pessoal é removida; você é desconectado em todos os lugares.",
        ),
        "delete.list_contributions" => (
            "Contributions you added may remain on the map, anonymized.",
            "As contribuições que você adicionou podem permanecer no mapa, anonimizadas.",
        ),
        "delete.list_private" => (
            "Private activity (favorites, \"I parked here\") is deleted.",
            "A atividade privada (favoritos, \"Estacionei aqui\") é excluída.",
        ),
        "delete.confirm" => ("Delete my account", "Excluir minha conta"),
        "admin.title" => ("Admin", "Admin"),
        "admin.privacy_requests.title" => ("Privacy requests", "Solicitações de privacidade"),
        "admin.privacy_requests.empty" => (
            "No privacy requests.",
            "Nenhuma solicitação de privacidade.",
        ),
        "admin.privacy_requests.subtitle" => (
            "Data-subject requests, oldest first. The deadline is 15 days from the request (LGPD art. 19).",
            "Solicitações de titulares, mais antigas primeiro. O prazo é de 15 dias a partir do pedido (LGPD art. 19).",
        ),
        "admin.privacy_requests.details" => ("What they asked for", "O que foi solicitado"),
        "admin.privacy_requests.no_details" => (
            "No further details were given.",
            "Nenhum detalhe adicional foi informado.",
        ),
        "admin.privacy_requests.days_left" => ("{n} days left", "{n} dias restantes"),
        "admin.privacy_requests.overdue" => ("{n} days overdue", "{n} dias em atraso"),
        "privacy.subject.anonymized" => ("Anonymized account", "Conta anonimizada"),
        "admin.privacy_requests.fulfill" => ("Mark completed", "Marcar como concluída"),

        // --- transactional email ------------------------------------------
        // Rendered by the `email.send` job handler in the recipient's stored
        // locale. `{app}` is the product name; `{link}` the single-use URL.
        "email.verify.subject" => ("Confirm your {app} email", "Confirme seu e-mail no {app}"),
        "email.verify.body" => (
            "Welcome to {app}. Confirm your email address to activate your account:\n\n{link}\n\nIf you did not create an account, you can ignore this email.",
            "Bem-vindo ao {app}. Confirme seu endereço de e-mail para ativar sua conta:\n\n{link}\n\nSe você não criou uma conta, pode ignorar este e-mail.",
        ),
        "email.reset.subject" => ("Reset your {app} password", "Redefina sua senha do {app}"),
        "email.reset.body" => (
            "We received a request to reset your {app} password. Choose a new one here:\n\n{link}\n\nIf you did not ask for this, you can safely ignore this email.",
            "Recebemos um pedido para redefinir sua senha do {app}. Escolha uma nova senha aqui:\n\n{link}\n\nSe não foi você que pediu, pode ignorar este e-mail com segurança.",
        ),
        "email.change.subject" => (
            "Confirm your new {app} email",
            "Confirme seu novo e-mail no {app}",
        ),
        "email.change.body" => (
            "Confirm this address to finish changing the email on your {app} account:\n\n{link}\n\nIf you did not ask for this change, ignore this email — your current address stays as it is.",
            "Confirme este endereço para concluir a troca de e-mail da sua conta no {app}:\n\n{link}\n\nSe você não pediu essa troca, ignore este e-mail — seu endereço atual continua o mesmo.",
        ),
        // Unknown key: a visible marker (all real keys are defined above, so
        // this only appears when a template references a typo'd key).
        _ => ("⟨i18n?⟩", "⟨i18n?⟩"),
    };
    match locale {
        Locale::En => en,
        Locale::PtBr => pt,
    }
}
/// The keys the transactional email renderer looks up. Listed here so the
/// tests below (and `bikesnest_infrastructure::email::templates`) fail loudly
/// when one is renamed away.
#[cfg(test)]
const EMAIL_KEYS: [&str; 6] = [
    "email.verify.subject",
    "email.verify.body",
    "email.reset.subject",
    "email.reset.body",
    "email.change.subject",
    "email.change.body",
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Every transactional-email key must exist in BOTH locales: a missing one
    /// degrades to the `⟨i18n?⟩` marker, and that marker would be *mailed to a
    /// user* rather than merely showing up on a page.
    #[test]
    fn email_keys_exist_in_both_locales() {
        for key in EMAIL_KEYS {
            for locale in [Locale::En, Locale::PtBr] {
                let text = msg(locale, key);
                assert_ne!(text, "⟨i18n?⟩", "{key} is missing for {}", locale.code());
                assert!(
                    !text.trim().is_empty(),
                    "{key} is empty for {}",
                    locale.code()
                );
            }
        }
    }

    /// The two columns must actually differ — an English string copy-pasted
    /// into the pt-BR column is the regression this catches.
    #[test]
    fn email_strings_are_translated_not_copied() {
        for key in EMAIL_KEYS {
            assert_ne!(
                msg(Locale::En, key),
                msg(Locale::PtBr, key),
                "{key} has the same text in both locales"
            );
        }
    }

    /// Every body must keep the `{link}` placeholder the renderer substitutes;
    /// losing it would send a verification mail with no way to verify.
    #[test]
    fn email_bodies_keep_the_link_placeholder() {
        for key in ["email.verify.body", "email.reset.body", "email.change.body"] {
            for locale in [Locale::En, Locale::PtBr] {
                assert!(
                    msg(locale, key).contains("{link}"),
                    "{key} lost its {{link}} placeholder for {}",
                    locale.code()
                );
            }
        }
    }

    #[test]
    fn locale_codes_round_trip() {
        // `LocaleCode::as_str()` in the domain emits "pt-BR"/"en"; both must
        // parse back here, because that is how a stored locale reaches the
        // catalog.
        assert_eq!(Locale::from_code("pt-BR"), Some(Locale::PtBr));
        assert_eq!(Locale::from_code("en"), Some(Locale::En));
        assert_eq!(Locale::from_code("EN"), Some(Locale::En));
        assert_eq!(Locale::from_code("fr"), None);
        assert_eq!(Locale::PtBr.html_lang(), "pt-BR");
        assert_eq!(Locale::PtBr.code(), "pt-br");
    }
}
