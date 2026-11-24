# Déployer le serveur GameSync

## Docker classique

Depuis la racine du dépôt, lancer `docker compose -f docker/compose.yaml up -d --build`.
Le volume Docker nommé conserve SQLite et les fichiers dédupliqués. Le port 8787 est publié sur la boucle locale de l’hôte. Pour modifier le port hôte, copier `docker/.env.example` puis passer le fichier à Compose avec `--env-file`.

## Unraid

Sur l’hôte, choisir un dossier appdata dédié sur un système de fichiers local. Pour une nouvelle installation, utiliser un dossier vide : GameSync prépare automatiquement ses droits au premier démarrage. Si le dossier contient déjà des données, elles ne sont pas modifiées automatiquement ; elles doivent être accessibles à l'UID/GID 10001.

### Installation depuis Community Apps

Le modèle Community Apps utilise l'image x86-64 `ghcr.io/mehdimp4/gamesync-server:latest` et ne fournit pas d'interface Web : l'administration des jeux se fait depuis le client desktop. Dans **Apps**, rechercher GameSync, choisir **Install** et conserver les valeurs proposées pour une première installation. Le dossier appdata doit être dédié à GameSync et vide au départ.

Le port du modèle est accessible sur le réseau de l'hôte : limiter son accès à un réseau de confiance. L'API du serveur est en HTTP ; utiliser un proxy HTTPS ou Tailscale Serve pour les clients et ne pas exposer directement le port sur Internet. Consulter les instructions de votre proxy pour son adresse amont.

Créer le premier code d'appairage dans le terminal Unraid (adapter `GameSync` si le conteneur a été renommé) :

```sh
docker exec GameSync gamesync-server invite --admin
```

Saisir ce code et l'adresse HTTPS du serveur dans le client desktop. Suivre ensuite la [recette](validation.md) avec des sauvegardes fictives. La validation sur un hôte Unraid reste à effectuer.

### Installation avec Docker Compose

```sh
docker compose -f docker/compose.yaml -f docker/compose.unraid.yaml up -d --build
```

Pour utiliser l'image préconstruite sur un hôte Unraid x86-64, télécharger l'artefact `gamesync-server-linux-amd64` d'une exécution réussie du workflow **Publish server image**, puis extraire l'archive de l'artefact GitHub. Depuis le dossier contenant les deux fichiers obtenus :

```sh
sha256sum -c gamesync-server-linux-amd64.tar.gz.sha256
docker load -i gamesync-server-linux-amd64.tar.gz
docker compose -f docker/compose.yaml -f docker/compose.unraid.yaml up -d --no-build
```

Les fichiers Compose doivent provenir du même commit que l'artefact. Utiliser un dossier appdata dédié et vide pour une nouvelle installation. Cette image est une archive Docker locale, pas une image publiée sur un registre ; elle doit être chargée à nouveau pour chaque mise à jour.

Les exemples ci-dessous emploient le déploiement Docker classique. Pour Unraid, ajouter le second `-f` aux mêmes commandes.

## Accès et diagnostics

Utiliser Tailscale Serve ou un proxy HTTPS pour relier l’adresse des clients au port local du serveur. Conserver ce port sur la boucle locale. Vérifier d’abord `curl -fsS http://127.0.0.1:8787/healthz`, puis `docker compose -f docker/compose.yaml logs --tail=100 gamesync`.

```sh
docker compose -f docker/compose.yaml exec gamesync gamesync-server invite --admin
docker compose -f docker/compose.yaml exec gamesync gamesync-server revoke UUID
```

Un code généré avec `--admin` accorde les droits d’administration au prochain appareil appairé. Générer les autres codes depuis **Appareils** ou avec la commande `invite` sans option.

Le tableau de bord du client affiche le nombre de jeux synchronisés, les octets des objets dédupliqués et les publications récentes. Les erreurs HTTP du serveur sont affichées pour le processus en cours et consignées dans les logs du conteneur. Le total des objets exclut SQLite et les envois temporaires.

## Sauvegarde du serveur

Arrêter le service avant de copier l’ensemble du volume ou du dossier appdata, y compris les fichiers SQLite auxiliaires et le dossier d’objets. Pour restaurer l’instance, remettre cette copie dans un volume ou dossier dédié avec les mêmes droits, puis redémarrer le service et vérifier l’état et l’accès d’un appareil appairé. Une copie de l’instance contient les empreintes d’authentification et les sauvegardes de tous les jeux.

Le test `bash server/tests/compose-smoke.sh` lance un projet Compose isolé, puis vérifie que le jeton, un objet et la politique de rétention restent disponibles après recréation du conteneur. Il supprime ensuite les données temporaires de ce projet.

Le test de recréation du conteneur a réussi sur Docker local. La recette sur un hôte Unraid reste à effectuer.

## Publier sur Community Apps

1. Rendre le dépôt GitHub public, puis pousser les changements du modèle `templates/gamesync.xml`, du profil `ca_profile.xml`, du serveur et des workflows. La licence MIT est déjà à la racine. Vérifier que les URL du modèle, du profil et de l'icône sont accessibles sans connexion.
2. Un push sur `main` qui touche le serveur, Docker ou une dépendance Rust du serveur lance automatiquement **Publish server image**. Le workflow teste l'image puis publie les tags `latest` et `sha-<commit>` sur GHCR. Vérifier le résultat dans **Actions**. Un lancement manuel reste possible sur `main`.
3. Dans les paramètres du package GitHub `gamesync-server`, rendre le package public si nécessaire. Vérifier anonymement que `ghcr.io/mehdimp4/gamesync-server:latest` peut être téléchargé. Si le propriétaire GitHub change, adapter les liens et la référence d'image dans le modèle.
4. Installer le modèle et l'image sur un véritable serveur Unraid, choisir un dossier appdata dédié et vide et suivre la [recette](validation.md). Vérifier en particulier l'appairage, la persistance après recréation et les conflits avec deux clients. Tester également l'installation du client Windows depuis un artefact ou une version publiée.
5. Ouvrir [Community Apps Submit](https://ca.unraid.net/submit), se connecter, démarrer une soumission et renseigner l'URL publique `https://github.com/MEHDImp4/GameSync`. Exécuter **Validate** puis **Scan**, examiner la fiche et résoudre les erreurs signalées avant de soumettre pour revue. Répéter ces contrôles après toute modification significative des fichiers XML.

Les artefacts GitHub Actions ne remplacent pas l'image publique du registre. La visibilité du package, la validation du portail, la recette Unraid et l'acceptation de la fiche nécessitent une vérification après publication.

## Rétention

Le serveur conserve les 20 dernières révisions publiées de chaque jeu ainsi que toutes celles datant de moins de 30 jours. Il préserve également la version courante, les variantes de conflit et leur révision de base. Les variables `GAMESYNC_KEEP_COUNT` et `GAMESYNC_KEEP_DAYS` dans le fichier d’environnement Compose définissent les valeurs initiales. Un appareil administrateur peut enregistrer une politique dans **Paramètres**, sous **Rétention du serveur**. Cette politique est conservée dans SQLite et prévaut sur les valeurs d’environnement, y compris après un redémarrage. Les identifiants expirés sont conservés pour reconnaître les nouvelles tentatives de publication.

Le nettoyage s’exécute quotidiennement lorsque le serveur tourne. Pour le déclencher manuellement, utiliser `docker compose -f docker/compose.yaml exec gamesync gamesync-server cleanup`. Seuls les objets sans référence et âgés d’au moins 24 heures sont collectés. Après résolution, la variante reste consultable au moins pendant la durée de rétention, calculée à partir de la résolution. Une variante de conflit ouverte et sa base restent protégées jusqu’à la clôture.

Les envois incomplets sont conservés sept jours sans nouveau bloc. Le serveur limite à quatre les corps d’envoi simultanément reçus et à huit les téléchargements ouverts. L’appairage accepte jusqu’à trente tentatives par minute pour l’ensemble de l’instance. Un client peut réessayer après la temporisation indiquée par le serveur.
