# Déployer le serveur GameSync

## Docker classique

Depuis la racine du dépôt, lancer `docker compose -f docker/compose.yaml up -d --build`.
Le volume Docker nommé conserve SQLite et les fichiers dédupliqués. Le port 8787 est publié sur la boucle locale de l’hôte. Pour modifier le port hôte, copier `docker/.env.example` puis passer le fichier à Compose avec `--env-file`.

## Unraid

Sur l’hôte, choisir un chemin appdata sur un système de fichiers local au serveur. Créer le dossier et attribuer son accès à l’UID/GID 10001 du conteneur, par exemple `install -d -o 10001 -g 10001 -m 0700 /mnt/user/appdata/gamesync`. Le dossier doit être réservé à GameSync.

```sh
docker compose -f docker/compose.yaml -f docker/compose.unraid.yaml up -d --build
```

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

## Rétention

Le serveur conserve les 20 dernières révisions publiées de chaque jeu ainsi que toutes celles datant de moins de 30 jours. Il préserve également la version courante, les variantes de conflit et leur révision de base. Les variables `GAMESYNC_KEEP_COUNT` et `GAMESYNC_KEEP_DAYS` dans le fichier d’environnement Compose définissent les valeurs initiales. Un appareil administrateur peut enregistrer une politique dans **Paramètres**, sous **Rétention du serveur**. Cette politique est conservée dans SQLite et prévaut sur les valeurs d’environnement, y compris après un redémarrage. Les identifiants expirés sont conservés pour reconnaître les nouvelles tentatives de publication.

Le nettoyage s’exécute quotidiennement lorsque le serveur tourne. Pour le déclencher manuellement, utiliser `docker compose -f docker/compose.yaml exec gamesync gamesync-server cleanup`. Seuls les objets sans référence et âgés d’au moins 24 heures sont collectés. Après résolution, la variante reste consultable au moins pendant la durée de rétention, calculée à partir de la résolution. Une variante de conflit ouverte et sa base restent protégées jusqu’à la clôture.

Les envois incomplets sont conservés sept jours sans nouveau bloc. Le serveur limite à quatre les corps d’envoi simultanément reçus et à huit les téléchargements ouverts. L’appairage accepte jusqu’à trente tentatives par minute pour l’ensemble de l’instance. Un client peut réessayer après la temporisation indiquée par le serveur.
