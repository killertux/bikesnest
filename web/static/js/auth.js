// Prefer the initiating form's current token. The pinned htmx keeps the old
// document head after a boosted body swap, so head metadata is only a fallback.
document.addEventListener('htmx:config:request', function (e) {
  var request = e.detail.ctx.request;
  if (/^(GET|HEAD|OPTIONS)$/i.test(request.method)) return;
  if (Object.prototype.hasOwnProperty.call(request.headers, 'X-CSRF-Token')) return;
  var input = request.form && request.form.querySelector('input[name="csrf"]');
  var token = input && input.value;
  if (!token && request.body && request.body.get) token = request.body.get('csrf');
  if (!token) token = document.body && document.body.dataset.csrf;
  if (!token) {
    var meta = document.querySelector('meta[name="csrf"]');
    token = meta && meta.getAttribute('content');
  }
  if (token) request.headers['X-CSRF-Token'] = token;
});

// Never replay a rejected mutation. Keep the live form and its entered values
// in place, then let the visitor deliberately reload or establish identity.
document.addEventListener('htmx:before:response', function (e) {
  var response = e.detail.ctx.response;
  if (!response || response.headers.get('x-bikesnest-csrf-recovery') !== 'reload-required') return;
  e.preventDefault();
  var old = document.getElementById('csrf-recovery');
  if (old) old.remove();
  var box = document.createElement('div');
  box.id = 'csrf-recovery';
  box.tabIndex = -1;
  box.setAttribute('role', 'alert');
  box.className = 'mx-auto mt-4 max-w-shell rounded-xl border border-danger/30 bg-danger/5 p-4 text-sm';
  var message = document.createElement('p');
  message.textContent = document.body.dataset.csrfError;
  var reload = document.createElement('a');
  reload.href = location.href;
  reload.textContent = document.body.dataset.csrfReload;
  reload.className = 'mr-4 font-medium underline';
  var login = document.createElement('a');
  login.href = '/login?next=' + encodeURIComponent(location.pathname + location.search);
  login.textContent = document.body.dataset.csrfLogin;
  login.className = 'font-medium underline';
  box.append(message, reload, login);
  var main = document.getElementById('content');
  if (main) main.before(box);
  box.focus();
});
