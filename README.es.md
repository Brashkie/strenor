# Strenor

*Léelo en [English](./README.md).*

Almacén clave-valor embebido y de alto rendimiento para Node.js, con un núcleo
escrito en Rust. **En proceso** — sin servidor que levantar, sin red, sin
configuración. `npm install`, importas y usas.

> ⚠️ **Alpha / experimental.** El formato y la API pueden cambiar antes de
> `0.1.0`. Instala con `npm install strenor@alpha`.

## Por qué

Redis es un servidor aparte: lo levantas, te conectas por un socket y serializas
todo por el cable, incluso en localhost. Strenor vive **dentro de tu proceso de
Node** como un addon nativo — cero latencia de red, cero configuración. Se parece
más a `better-sqlite3` que a Redis: la herramienta correcta cuando un solo
proceso necesita almacenamiento local rápido.

Nació de una necesidad real: darle a WinsiBot almacenamiento local rápido sin
depender de un servicio externo.

## Qué es (y qué no), hoy

- ✅ Almacén KV embebido en proceso (un solo proceso).
- ✅ Formato de valor binario etiquetado: `Buffer`, `string` y objetos en JSON.
- ✅ TTL con expiración perezosa + barrido en segundo plano opcional.
- ✅ Snapshot binario auto-descriptivo (`dump`/`load`).
- ✅ Codec de objetos intercambiable (JSON por defecto; msgpack/cbor se acoplan).
- ❌ Sin Pub/Sub, sin compartir entre procesos, sin modo servidor (posible más
  adelante).
- ❌ El snapshot es un volcado completo, no un log append-only (aún sin escritura
  resistente a caídas).
- ⚠️ El codec JSON por defecto hereda los límites de JSON: `Date` pasa a string,
  `undefined` se descarta, `BigInt` lanza error y las funciones se ignoran. Usa
  un codec más rico (msgpack/cbor) si los necesitas.

## Uso

```js
const { Strenor } = require('strenor');
// o, con ESM / TypeScript:  import { Strenor } from 'strenor';

const db = new Strenor();

// API inteligente — despacha según el tipo
db.set('user:1', { name: 'Brashkie', age: 20 });
db.get('user:1'); // -> { name: 'Brashkie', age: 20 }

db.set('hello', 'world');
db.get('hello'); // -> 'world'

db.set('avatar', algunBuffer);
db.get('avatar'); // -> Buffer

// TTL (milisegundos)
db.set('session', token, { ttl: 60_000 });
db.ttl('session'); // ms restantes, -1 sin expiración, -2 no existe

// Persistencia
db.dump('./strenor.snap');
db.load('./strenor.snap');
```

### Codec personalizado

```js
// Un codec es { tag, encode(value) -> Buffer, decode(bytes) -> value }
// Los tags personalizados van en 0x20..0xFE.
const msgpackCodec = {
  tag: 0x20,
  encode: (v) => Buffer.from(encode(v)),
  decode: (b) => decode(b),
};

const db = new Strenor({ codec: msgpackCodec });
db.registerCodec(msgpackCodec); // para que los valores ya etiquetados decodifiquen
```

## Formato de valor

Cada valor guardado es `[tag:u8][payload]`. El núcleo en Rust nunca lo
interpreta — la decodificación vive por completo en la capa JS, lo que hace que
el snapshot en disco sea auto-descriptivo y el formato seguro de extender.

| Tag         | Significado             |
| ----------- | ----------------------- |
| `0x00`      | Bytes crudos (Buffer)   |
| `0x01`      | String UTF-8            |
| `0x02`      | JSON                    |
| `0x03–0x06` | Reservado               |
| `0x20–0xFE` | Codecs intercambiables  |

## Estructura del proyecto

```
strenor/
├── Cargo.toml             # en la raíz (convención napi)
├── build.rs
├── crates/
│   └── lib.rs             # núcleo Rust: store agnóstico + TTL + snapshot
├── src/                   # TypeScript (puro)
│   ├── index.ts           # API pública: tags, codecs, helpers, sweeper de TTL
│   └── native.ts          # loader tipado del .node compilado
├── __tests__/             # suite Vitest (instrumenta src/)
├── scripts/
│   └── smoke.ts           # prueba end-to-end (tsx)
├── .github/workflows/     # CI + release
├── biome.json             # lint + format
├── vitest.config.ts       # tests + coverage v8
├── tsup.config.ts         # bundler: CJS + ESM + .d.ts
├── tsconfig.json          # type-check estricto (TS6, noEmit)
└── package.json
```

## Compilar y probar

```bash
npm install
# compila el addon nativo (Vekziun, o `cargo build --release`)
# y deja strenor.node en la raíz del paquete
npm run build         # tsup -> dist/index.js (ESM) + index.cjs (CJS) + index.d.ts
npm run typecheck     # tsc (TS6)
npm run test:coverage # vitest + coverage v8 (instrumenta src/)
npm run smoke         # verificación rápida end-to-end contra el bundle
```

Lint / formato con Biome: `npm run check`, `npm run format`.

> El `Cargo.toml` está en la raíz (convención napi); su `[lib] path` apunta a
> `crates/lib.rs`, así `src/` queda puro TypeScript. Apunta Vekziun al
> `Cargo.toml` de la raíz. El `.node` compilado debe quedar en la raíz del
> paquete como `strenor.node`; `src/native.ts` lo resuelve. Los workflows de
> CI/release compilan un binario Linux como base — la distribución
> multiplataforma real va por Vekziun.

## Licencia

Apache-2.0 © Hepein Oficial (Brashkie)
