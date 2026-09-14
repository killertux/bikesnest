Esta Política de Cookies descreve cookies, preferências locais e telemetria de provedores usados com o BikesNest. Ela complementa a [Política de Privacidade](/privacy).

## O que usamos

O aplicativo usa cookies próprios para autenticação, proteção de formulários e preferência de idioma. As preferências locais e o tratamento por provedores externos são descritos separadamente abaixo.

| Cookie | Finalidade | Duração | Tipo | Atributos |
|---|---|---|---|---|
| `session_id` | Mantém você autenticado após o login | 30 dias sem uso (máximo 90 dias) | Necessário | HttpOnly, Secure, SameSite=Lax |
| `csrf` | Protege formulários contra falsificação de requisição entre sites (CSRF) enquanto você não está autenticado | 1 hora | Necessário — segurança | HttpOnly, SameSite=Lax |
| `lang` | Guarda o idioma que você escolheu (português ou inglês); só é gravado quando você troca o idioma | 1 ano | Funcional | SameSite=Lax |

A preferência de mapa aberto fica no *localStorage* sob `bn.search.mapOpen`. Ela lembra se você abriu ou fechou o mapa da busca, não tem expiração definida pelo aplicativo e muda quando você alterna essa preferência. Você pode removê-la limpando o armazenamento deste site no navegador; o próprio navegador também pode removê-la. O aplicativo não envia essa preferência como cookie. Bloquear o armazenamento local não impede o botão do mapa de funcionar, mas a preferência pode não ser lembrada.

O Cloudflare Web Analytics é usado para medir visualizações de páginas e desempenho por meio de um beacon no navegador. Essas medições são separadas da preferência local do mapa e dos nossos cookies de autenticação. A Cloudflare descreve esse serviço como focado em privacidade; consulte [Cloudflare Web Analytics](https://developers.cloudflare.com/web-analytics/about/). A presença de análise de uso, por si só, não comprova o armazenamento de um cookie de análise.

## Cookies de terceiros

O BikesNest não usa cookies de terceiros para publicidade ou rastreamento. Ao exibir o mapa, seu navegador contata diretamente o provedor selecionado; essa requisição leva seu endereço IP, e o provedor trata o armazenamento ou os cookies próprios conforme sua política de privacidade. Veja a seção "Com quem compartilhamos" da [Política de Privacidade](/privacy).

## Como controlar

Você pode apagar ou bloquear cookies nas configurações do seu navegador. Sem o cookie `session_id` não é possível permanecer conectado; sem o cookie `csrf` os formulários públicos (como cadastro e login) não funcionam.

## Alterações

Atualizaremos esta Política quando nossas práticas de armazenamento no navegador ou telemetria mudarem. Quando a lei aplicável exigir consentimento para tecnologias opcionais, nós o solicitaremos antes de ativá-las. As versões anteriores ficam em [/cookies/versions](/cookies/versions).

Contato: **{{CONTACT_EMAIL}}**.
