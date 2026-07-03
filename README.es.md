<div align="center">

<img src="https://raw.githubusercontent.com/Brashkie/strenor/main/media/strenor.png" alt="Strenor" width="360" />

### Almacén clave-valor embebido y de alto rendimiento para Node.js, con núcleo en Rust

<em>En proceso&nbsp; · &nbsp;Sin servidor&nbsp; · &nbsp;Sin red&nbsp; · &nbsp;Sin configuración</em>

<br />

[![npm](https://img.shields.io/npm/v/strenor/alpha.svg?color=cb3837&label=npm)](https://www.npmjs.com/package/strenor)
[![node](https://img.shields.io/badge/node-%3E%3D18-339933.svg?logo=node.js&logoColor=white)](https://nodejs.org)
[![rust](https://img.shields.io/badge/core-Rust-dea584.svg?logo=rust&logoColor=white)](https://www.rust-lang.org)
[![coverage](https://img.shields.io/badge/coverage-100%25-brightgreen.svg)](#)
[![types](https://img.shields.io/badge/types-included-3178c6.svg?logo=typescript&logoColor=white)](./dist/index.d.ts)
[![license](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

<br />

**[Inicio rápido](#inicio-rápido)&nbsp; · &nbsp;[Ejemplos](#ejemplos)&nbsp; · &nbsp;[API](#api)&nbsp; · &nbsp;[Comparación](#comparación)&nbsp; · &nbsp;[Roadmap](./ROADMAP.md)&nbsp; · &nbsp;[Ecosistema](./ECOSYSTEM.md)**

<sub>Read this in <a href="./README.md">English</a></sub>

</div>

<br />

> [!WARNING]
> **Alpha / experimental.** El formato y la API aún pueden cambiar antes de
> `0.1.0`. Instala con `npm install strenor@alpha`. Se agradecen issues y feedback.

<details>
<summary><b>Tabla de contenidos</b></summary>

- [Por qué Strenor](#por-qué-strenor) · [Cuándo usarlo](#cuándo-usarlo) · [Instalación](#instalación)
- [Inicio rápido](#inicio-rápido) · [Ejemplos](#ejemplos) · [Comparación](#comparación)
- [API](#api) · [Formato de valor](#formato-de-valor) · [Cómo funciona](#cómo-funciona) · [Rendimiento](#rendimiento)
- [Compilación multiplataforma](#compilación-multiplataforma-y-publicación) · [Estructura del proyecto](#estructura-del-proyecto)
- [Roadmap](./ROADMAP.md) · [Ecosistema](./ECOSYSTEM.md) · [Contribuir](#contribuir) · [Licencia](#licencia)

</details>

---

## Por qué Strenor

Redis es un **servidor aparte**. Incluso en localhost levantas un proceso, abres
un socket y serializas cada valor por el cable — dos veces por ida y vuelta. Para
una app o bot de un solo proceso, eso es latencia y peso operativo que pagas por
capacidades que quizá no uses.

Strenor vive **dentro de tu proceso de Node** como un addon nativo escrito en
Rust. No hay servidor que arrancar, ni puerto, ni salto de red, ni pool de
conexiones. Haces `npm install`, importas y llamas métodos — el store está ahí,
en memoria, con la opción de persistir a disco.

El modelo mental se parece más a **`better-sqlite3`** que a Redis: la herramienta
correcta cuando un proceso necesita almacenamiento local rápido, no cuando muchos
servicios deben compartir estado por red.

Strenor nació de una necesidad real: darle al bot de WhatsApp **WinsiBot**
almacenamiento local rápido sin operar un servicio externo.

## Cuándo usarlo

Strenor encaja cuando:

- Corres un **solo proceso de Node** (un bot, una CLI, un worker, una edge
  function) que necesita acceso clave-valor rápido.
- Quieres **cero configuración** para caché, sesiones o estado efímero con TTL.
- Quieres **persistencia local** (sobrevivir reinicios) sin levantar una base de datos.
- Guardas **blobs binarios** (avatares, miniaturas, payloads serializados) y no
  quieres pasarlos en base64 por un protocolo de red.

Strenor **no** es la herramienta cuando:

- Varios procesos o servicios deben **compartir** el mismo store -> usa Redis.
- Necesitas **Pub/Sub**, replicación o clustering -> usa Redis.
- Necesitas **consultas relacionales** o transacciones entre tablas -> usa SQLite/Postgres.

## Instalación

```bash
npm install strenor@alpha
```

Se distribuyen binarios nativos precompilados por plataforma, así que el
consumidor no compila nada. Plataformas soportadas: Windows, macOS (x64/arm64) y
Linux (glibc **y** musl), en x64 y arm64.

## Inicio rápido

```ts
import { Strenor } from 'strenor';
// CommonJS:  const { Strenor } = require('strenor');

const db = new Strenor();

// API inteligente — despacha según el tipo en runtime
db.set('user:1', { name: 'Brashkie', age: 20 }); // objeto -> JSON
db.set('token', 'abc123'); //                        string -> UTF-8
db.set('avatar', pngBuffer); //                      Buffer -> bytes crudos

db.get('user:1'); // -> { name: 'Brashkie', age: 20 }
db.get('token'); //  -> 'abc123'
db.get('avatar'); // -> Buffer

// TTL en milisegundos
db.set('session:42', sessionData, { ttl: 30 * 60_000 });
db.ttl('session:42'); // ms restantes (-1 = sin expiración, -2 = no existe)

// Persistir a disco y recargar
db.dump('./strenor.snap');
db.load('./strenor.snap');
```

## Ejemplos

### Caché con expiración

```ts
const cache = new Strenor({ sweepInterval: 60_000 }); // barre vencidos cada 60s

function getUser(id: string) {
  const hit = cache.get<User>(`user:${id}`);
  if (hit) return hit;
  const user = fetchUserFromApi(id);
  cache.set(`user:${id}`, user, { ttl: 5 * 60_000 }); // cachea 5 minutos
  return user;
}
```

### Store de sesiones para un bot

```ts
const sessions = new Strenor();

function touchSession(userId: string, state: SessionState) {
  sessions.set(`sess:${userId}`, state, { ttl: 30 * 60_000 });
}

// Persistir al apagar, restaurar al arrancar
process.on('SIGTERM', () => sessions.dump('./sessions.snap'));
try {
  sessions.load('./sessions.snap');
} catch {
  /* primer arranque: aún no hay snapshot */
}
```

### Valores binarios

```ts
db.setBuffer('thumb:1', await sharp(input).resize(128).toBuffer());
const thumb = db.getBuffer('thumb:1'); // Buffer, byte a byte
```

### Codec personalizado (msgpack, cbor, ...)

El codec de objetos por defecto es JSON. Cámbialo por uno que preserve más tipos:

```ts
import { encode, decode } from '@msgpack/msgpack';

const msgpack = {
  tag: 0x20, // los tags personalizados van en 0x20..0xFE
  encode: (v: unknown) => Buffer.from(encode(v)),
  decode: (b: Buffer) => decode(b),
};

const db = new Strenor({ codec: msgpack });
db.registerCodec(msgpack); // para que los valores msgpack ya escritos decodifiquen
```

## Dónde encaja Strenor

Strenor es un **almacén clave-valor embebido**. Sus verdaderos pares son otros
motores KV embebidos, no bases de datos en red ni analíticas.

- **Competencia directa** (KV embebido): LMDB, RocksDB, LevelDB, sled,
  `better-sqlite3` *usado solo como almacén KV*.
- **Solapamiento parcial**: SQLite y Redis, para el caso común en que solo
  necesitas `set` / `get`. Si un bot guarda sesiones con `SELECT data FROM
  sessions WHERE id = ?` o una ida y vuelta a Redis, Strenor hace lo mismo con
  `db.get(id)` — sin esquema, sin servidor, sin puerto.
- **No competencia**: DuckDB, PostgreSQL, MySQL, MongoDB. Consultas analíticas,
  joins relacionales y servidores multi-nodo son otros problemas que Strenor no
  intenta resolver.

## Comparación

| | **Strenor** | LMDB / RocksDB | better-sqlite3 | Redis | SQLite |
|---|---|---|---|---|---|
| Categoría | KV embebido | KV embebido | SQL embebido | KV servidor | SQL embebido |
| En proceso | sí | sí | sí | no (servidor) | sí |
| Sobrecarga de red | ninguna | ninguna | ninguna | socket + protocolo | ninguna |
| API nativa de Node | sí (addon Rust) | vía bindings | sí (addon C) | vía cliente | sí (addon C) |
| Valores | bytes + codecs | bytes | filas SQL / BLOB | strings/estructuras | filas SQL / BLOB |
| Ergonomía `get`/`set` | `db.get(id)` | cursor/txn | sentencia SQL | round trip a cliente | sentencia SQL |
| TTL / expiración | integrado | manual | manual | integrado | manual |
| Persistencia a disco | snapshot (AOF planeado) | motor completo | BD completa | RDB/AOF | BD completa |
| Setup / operación | ninguno | ninguno | ninguno | levantar servidor | ninguno |

Strenor cambia las capacidades de red, multiproceso y relacionales de los demás
por cero configuración y acceso en proceso sin latencia. Elígelo cuando un
proceso necesita un store local rápido; elige los otros cuando necesites lo que
ellos añaden.

## API

Construcción:

```ts
new Strenor(options?: {
  codec?: Codec;          // codec de objetos por defecto (default: JSON)
  sweepInterval?: number; // ms; barrido en segundo plano de vencidos (unref'd)
})
```

Operaciones núcleo (todas síncronas):

| Método | Descripción |
|---|---|
| `set(key, value, opts?)` | Guarda cualquier valor; `opts.ttl` (ms), `opts.codec` para forzar |
| `get<T>(key)` | Lee y decodifica por tag; `null` si no existe/venció |
| `setString` / `getString` | Fuerza/asegura un string UTF-8 |
| `setBuffer` / `getBuffer` | Fuerza/asegura un `Buffer` crudo |
| `setJSON` / `getJSON` | Fuerza el codec de objetos explícitamente |
| `del(key)` | Borra; devuelve `true` si existía |
| `exists(key)` | Si existe una clave viva (no vencida) |
| `expire(key, ttlMs)` | Fija/reemplaza el TTL de una clave |
| `persist(key)` | Quita el TTL de una clave |
| `ttl(key)` | ms restantes; `-1` sin expiración, `-2` no existe |
| `keys()` | Todas las claves vivas (O(n); para sets chicos/debug) |
| `size()` | Número de entradas |
| `clear()` | Borra todo |
| `sweep()` | Purga vencidos activamente; devuelve cuántos quitó |
| `dump(path)` / `load(path)` | Snapshot a / desde un archivo auto-descriptivo |
| `registerCodec(codec)` | Registra un codec extra para un tag personalizado |
| `close()` | Detiene el sweeper en segundo plano (si lo hay) |

Un `Codec` es `{ tag, encode(value) => Buffer, decode(bytes) => value }` con un
byte `tag` en `0x20..0xFE`. Las definiciones de tipos van en `dist/index.d.ts`.

## Formato de valor

Cada valor guardado es `[tag:u8][payload]`. El **núcleo Rust nunca interpreta los
bytes** — la decodificación vive por completo en la capa TypeScript. Esto hace que
el snapshot en disco sea auto-descriptivo (cada valor sabe cómo decodificarse) y
el formato seguro de extender sin romper datos existentes.

| Tag | Significado |
|---|---|
| `0x00` | Bytes crudos (`Buffer`) |
| `0x01` | String UTF-8 |
| `0x02` | JSON (codec de objetos por defecto) |
| `0x03–0x06` | Reservado |
| `0x20–0xFE` | Codecs intercambiables (msgpack, cbor, ...) |

## Cómo funciona

Tres capas, cada una con una tarea:

1. **Núcleo Rust (`crates/lib.rs`)** — un mapa en memoria de `key -> (bytes,
   expiración)` protegido por un lock. No sabe nada de tipos; guarda bytes opacos
   `[tag][payload]`, maneja TTL (expiración perezosa + `sweep`) y lee/escribe el
   snapshot binario. Pequeño, rápido y agnóstico al valor por diseño.
2. **Capa TypeScript (`src/index.ts`)** — la API cómoda. Es dueña del contrato de
   tags y de los codecs, despacha `set`/`get` según el tipo, y gestiona el sweeper
   opcional.
3. **Loader nativo (`src/native.ts`)** — un loader pequeño y autocontenido que
   resuelve el `.node` correcto para la plataforma actual con la convención de
   nombres de @napi-rs/cli, distinguiendo glibc de musl para que el addon no
   falle en silencio en Alpine/Docker.

## Rendimiento

La historia de rendimiento de Strenor es estructural, no marketing con benchmarks:

- **Sin red, sin protocolo.** Las operaciones son llamadas directas en proceso a
  Rust — sin socket, sin framing de peticiones, sin serialización por un cable.
- **Síncrono por diseño.** Lecturas y escrituras al mapa en memoria retornan de
  inmediato, evitando idas y vueltas al event loop en los caminos calientes (la
  misma razón por la que `better-sqlite3` es rápido).
- **Una serialización, en la frontera.** Los valores se codifican una vez al
  entrar al store y se decodifican una vez al salir; el núcleo solo mueve bytes.

Un harness de benchmark reproducible (contra Redis-en-localhost y cachés en JS
puro) está en el roadmap. Hasta entonces no se afirman números aquí — mide contra
tu propia carga.

## Compilación multiplataforma y publicación

La distribución nativa usa [**@napi-rs/cli**](https://napi.rs). El campo `napi`
del `package.json` declara los triples objetivo; cada plataforma se publica como
su propio paquete npm, enlazado al principal vía `optionalDependencies`, así el
consumidor solo hace `npm install strenor` y obtiene el binario correcto.

Desarrollo local (solo target del host):

```bash
npm run build:native   # napi build --platform --release -> strenor.<suffix>.node
npm run build:ts       # tsup -> dist (ESM + CJS + tipos)
npm run test:coverage
```

El release multiplataforma se dispara por tag y corre en CI: una matriz compila
cada target (cross-compilando musl/arm64 con zig), y un job de publicación
recolecta los binarios, empaqueta los paquetes por plataforma y los publica
(primero los de plataforma, el principal al final). Ver
[`.github/workflows/release.yml`](./.github/workflows/release.yml).

Targets soportados: Windows (x64/arm64), macOS (x64/arm64), Linux glibc y musl
(x64/arm64), y Android (arm64/armv7).

## Estructura del proyecto

```
strenor/
├── Cargo.toml             # en la raíz (convención napi)
├── build.rs
├── crates/
│   └── lib.rs             # núcleo Rust: store agnóstico + TTL + snapshot
├── src/                   # TypeScript (puro)
│   ├── index.ts           # API pública: tags, codecs, helpers, sweeper de TTL
│   └── native.ts          # loader nativo (convención napi-rs)
├── __tests__/             # suite Vitest (instrumenta src/)
├── examples/               # runnable usage examples
├── scripts/
│   └── smoke.ts           # prueba end-to-end (tsx)
├── .github/workflows/     # CI + release multiplataforma
├── tsup.config.ts         # bundler: ESM + CJS + .d.ts
├── biome.json             # lint + format
├── vitest.config.ts       # tests + coverage v8
├── tsconfig.json          # type-check estricto (TS6)
└── package.json
```

## Roadmap

Strenor sigue una filosofía de "publica lo que funciona, luego crece". Lo clave:

- **v0.0.x — Núcleo alpha** *(publicado)*: KV, tags/codecs, TTL, snapshot, nativo multiplataforma.
- **v0.1.x — Primitivas para bots**: `list` / `queue` / `stack` / `deque`, `incr`/`decr` atómicos.
- **v0.2.x — Persistencia**: log append-only (AOF), recuperación de fallos, compactación, checksums.
- **v0.3.x — Estructuras**: `hash`, `set`, `sorted set`; codecs MsgPack/CBOR integrados.
- **v0.4.x — Transacciones**: `batch()`, `transaction()`, compare-and-swap.
- **v0.5.x — Rendimiento**: lecturas zero-copy, memory pools, benchmarks públicos.

El objetivo de la **v1.0** es confianza, no funciones: API estable, formato de
snapshot estable, benchmarks públicos y soporte completo Windows/Linux/macOS/
Android (x64/arm64).

**No-objetivos:** convertirse en SQLite, PostgreSQL, DuckDB o un cluster de Redis.

→ Roadmap completo por fases: **[ROADMAP.md](./ROADMAP.md)**

## Ecosistema

Hoy Strenor es un **solo paquete**: `strenor` (el núcleo) más paquetes nativos
por plataforma resueltos automáticamente vía `optionalDependencies`. El tooling
planeado vive bajo el scope `@strenor/*` (`@strenor/cli`, `@strenor/bench`,
`@strenor/inspector`, `@strenor/backup`) — se añade solo cuando una pieza tiene
un consumidor real por separado.

Strenor es una responsabilidad clara dentro de un conjunto más amplio de
proyectos independientes: **Vekziun** (tooling de build nativo) → **Strenor** (KV
embebido) → **signalis-core** (cripto) → **Signalis** (Signal Protocol) →
**HepeinBaileys** (WhatsApp). Su primer consumidor real es **WinsiBot**.

→ Ecosistema completo, modelo de paquetes y plan `@strenor/*`: **[ECOSYSTEM.md](./ECOSYSTEM.md)**

## Contribuir

Las contribuciones son bienvenidas. Ver [CONTRIBUTING.md](./CONTRIBUTING.md). En
resumen: `npm run check` (Biome), `npm run typecheck` y `npm run test:coverage`
deben pasar, y todo comportamiento nuevo necesita tests.

## Licencia

Apache-2.0 © Hepein Oficial (Brashkie)
