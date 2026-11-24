<div align="center">

# 🎮 GameSync

### Vos sauvegardes vous suivent, d’un PC à l’autre.

Jouez sur Windows ou Linux, retrouvez votre progression sur vos autres machines et gardez vos données chez vous grâce à un serveur auto-hébergé.

[Installer GameSync](#-bien-démarrer) · [Découvrir les fonctionnalités](#-pensé-pour-vos-parties) · [Documentation](#-documentation) · [Développement](#-développement)

</div>

---

> **Projet expérimental** — Le client et le serveur sont en cours de développement. Les tests graphiques ont été exécutés sous Linux et le serveur testé avec Docker ; la validation native sur Windows et le déploiement sur Unraid restent à effectuer. Commencez avec des sauvegardes fictives et gardez une copie indépendante de vos données.

## 🕹️ Une partie, plusieurs machines

Un jeu installé sur votre PC fixe et sur votre portable ? GameSync vous aide à retrouver vos sauvegardes sur les deux. Choisissez les dossiers à suivre, connectez vos appareils à votre serveur et lancez une synchronisation, manuellement ou en arrière-plan.

Vos machines peuvent utiliser des chemins différents : chaque appareil associe les dossiers du jeu à ses propres emplacements. Un jeu peut contenir jusqu’à 16 dossiers de sauvegarde.

## ✨ Pensé pour vos parties

- **Vos données, votre serveur.** Hébergez GameSync chez vous et appairez vos appareils avec un code à usage unique.
- **Votre progression à portée de main.** Synchronisez vos jeux manuellement ou activez le suivi en arrière-plan.
- **Un historique pour revenir en arrière.** Consultez les révisions et republiez une ancienne version si nécessaire.
- **Les conflits restent visibles.** Lorsque deux versions divergent, GameSync conserve une variante et vous laisse choisir comment résoudre le conflit.
- **Moins de configuration.** Découvrez vos jeux Steam et, sous Windows, les installations Epic et GOG. Un manifeste Ludusavi local peut suggérer des dossiers de sauvegarde à confirmer.
- **Des transferts vérifiés.** Les fichiers sont contrôlés avec BLAKE3 et les transferts interrompus peuvent reprendre.

## 🚀 Bien démarrer

### 1. Préparez votre serveur

Déployez le serveur avec Docker, puis créez un code d’appairage administrateur. Le [guide Docker et Unraid](docs/unraid.md) détaille les commandes, la persistance des données et l’accès HTTPS. Vous pouvez aussi suivre le [guide Android](docs/android.md) pour un serveur expérimental sous Termux.

### 2. Installez le client

Téléchargez l’installateur Windows depuis les [versions GitHub](https://github.com/MEHDImp4/GameSync/releases). Les builds de développement produisent également des paquets Debian et AppImage pour Linux. Consultez le [guide d’installation](docs/install.md) pour les paquets, la vérification des sommes SHA-256 et la compilation.

L’installateur Windows de développement n’est pas signé et peut déclencher Microsoft SmartScreen. L’accès aux versions d’un dépôt privé nécessite un compte autorisé.

### 3. Retrouvez vos sauvegardes

1. Dans **Paramètres**, saisissez l’adresse HTTPS de votre serveur, le nom de l’appareil et le code d’appairage.
2. Ajoutez un jeu de test et indiquez ses dossiers de sauvegarde et son exécutable.
3. Fermez le jeu, puis synchronisez l’appareil qui possède la sauvegarde initiale.
4. Appairez un second appareil, choisissez le jeu dans **Jeux sur le serveur** et renseignez ses chemins locaux.

Pour un premier essai complet, suivez la [checklist de validation](docs/validation.md). Le client vérifie le processus configuré avant d’appliquer une révision ; un lancement externe peut toutefois coïncider avec une opération déjà engagée.

## 🧭 Comment ça marche ?

```text
 PC Windows ou Linux       Votre serveur          PC Windows ou Linux
┌───────────────────┐     ┌─────────────┐        ┌───────────────────┐
│ Client GameSync   │ ◄─► │  GameSync   │ ◄────► │ Client GameSync   │
│ Sauvegardes locales│     │ Révisions   │        │ Sauvegardes locales│
└───────────────────┘     └─────────────┘        └───────────────────┘
```

Le serveur conserve les fichiers et les révisions ; chaque client garde ses chemins locaux et son jeton d’appareil. En cas de conflit, la synchronisation du jeu est mise en attente jusqu’à sa résolution. Les détails et les limites du protocole sont décrits dans l’[architecture](docs/architecture.md).

## 📚 Documentation

| Guide | Pour quoi faire ? |
| --- | --- |
| [Installation et mise à jour](docs/install.md) | Installer les clients, synchroniser un premier jeu et construire les paquets |
| [Docker et Unraid](docs/unraid.md) | Déployer, administrer et sauvegarder le serveur |
| [Android avec Termux](docs/android.md) | Essayer le serveur sur Android, sans application Android graphique |
| [Dépannage](docs/troubleshooting.md) | Diagnostiquer les problèmes courants |
| [Validation](docs/validation.md) | Tester Windows et Unraid avec des données fictives |
| [Architecture](docs/architecture.md) | Comprendre le stockage, la synchronisation et leurs garanties |
| [OpenAPI](server/openapi.yaml) | Explorer l’API du serveur, également servie sous `/openapi.yaml` |

## 🛠️ Développement

Le projet associe un serveur et des bibliothèques Rust à un client desktop Tauri, React et TypeScript.

```sh
# Tester le workspace Rust
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings

# Démarrer le serveur local sur 127.0.0.1:8787
cargo run --locked -p gamesync-server

# Démarrer le client desktop
cd apps/desktop
npm ci
npm run tauri -- dev
```

Le serveur utilise `GAMESYNC_PORT` et `GAMESYNC_DATA` pour configurer son port et son répertoire de données. Pour créer le premier code administrateur, utilisez `cargo run --locked -p gamesync-server -- invite --admin` avec le même répertoire de données que le serveur. Les prérequis et commandes de construction sont détaillés dans le [guide d’installation](docs/install.md).

Le workflow GitHub Actions construit les clients Windows et Linux et teste le serveur lorsque les sources, les fichiers de build ou le workflow changent. Une modification limitée à la documentation ne déclenche pas ces builds. Les tags et les lancements manuels déclenchent également le workflow ; les paquets et leurs sommes SHA-256 sont disponibles dans ses artefacts.

## Licence

GameSync est distribué sous [licence MIT](LICENSE).
